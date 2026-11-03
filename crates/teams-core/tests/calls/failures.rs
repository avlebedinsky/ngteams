//! The native signaling feeds the fence's call registry (a start's answer registers its call, its acceptance binds the
//! call controller's links, a follow-up of a call the fence didn't register is refused before it is sent, an answer or
//! an acceptance that doesn't read binds nothing, a start sent again after a throttle is the call its acceptance binds
//! to), and what a failure does as Task 14 decided with Codex (a media setup that fails refuses the action and sends
//! nothing, an answer's keeps the invitation ringing; the media closing under the call fails it with a `leave`; a
//! throttled write hung up during its wait isn't sent again; an `attach` refused with a 401 goes again and its
//! invitation is answered); the microphone opens only once a start passed the fence;
//! the engine stopping hangs the call up; a test call of an engine that doesn't know the account's tenant reads it
//! from the account's token first, and is refused, nothing sent, when the token names none.

use std::sync::Arc;
use std::time::Duration;

use crate::support::{A_TONE, B_TONE, Lab, SharedLog, TALK, tokens, wait_until};
use serde_json::json;
use teams_core::engine::EngineError;
use teams_core::{CallAction, CallActionError, CallStatus, CallTried, End, Outbox};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, Roster};
use teams_mock::{AX_CHAT, CHAT_BASE, ECHO, LAB_A, LAB_B, LAB_C, TENANT};
use teams_proto::{AcknowledgeMediaAnswer, Author, CallContext, CallLink, HttpTransport};

/// A media answer's acknowledgement on a call no start of this run registered.
const UNREGISTERED: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/mediaAnswers/MockCallNobodyStarted/1/t/5/acknowledge?i=10-0-0-1";

fn test_call() -> CallAction {
    CallAction::TestCall { echo: Mri::new(ECHO) }
}

/// A's outbox on the mock, its fence logging to `log`: deciding calls by the call registry when `tracking` (its
/// own rule, which no build's fence decides by: the strictest a call's writes are held to), by the lab's own rule
/// otherwise.
fn outbox_of(lab: &Lab, log: &SharedLog, tracking: bool) -> Outbox<HttpTransport<crate::support::Tokens>> {
    let fence = Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), [Mri::new(ECHO)]),
        FenceLog::from_writer(log.clone()),
    )
    .with_own_account(Mri::new(LAB_A));
    let fence = Arc::new(if tracking { fence.tracking_calls() } else { fence });
    let tokens = Arc::new(tokens(lab, LAB_A));
    let transport =
        Arc::new(HttpTransport::with_mock_origin(tokens, Arc::clone(&fence), CHAT_BASE, &lab.mock.origin()).unwrap());
    let members: Arc<dyn MembershipSource + Send> =
        Arc::new(LiveMembershipSource::with_mock_origin(&lab.mock.origin()).unwrap());
    Outbox::new(transport, fence, members, Author::new(Mri::new(LAB_A), "Lab A"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn under_the_lab_rule_a_follow_up_goes_with_its_permit_whatever_the_registry_holds() {
    let lab = Lab::start().await;
    let log = SharedLog::default();
    let outbox = outbox_of(&lab, &log, false);
    let write =
        AcknowledgeMediaAnswer::new(&CallContext::fresh().unwrap(), &CallLink::parse(UNREGISTERED).unwrap()).unwrap();
    let tried = outbox.try_call(write).await;
    assert!(matches!(tried, CallTried::Answered(_)), "{tried:?}");
    assert!(log.denials().is_empty());
    assert!(log.permitted_any("media_acknowledge"), "{:#?}", log.permitted());
    let sent = lab.mock.received_requests().await;
    assert!(
        sent.iter()
            .any(|request| request.url.path().contains("MockCallNobodyStarted")),
        "it went out"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_follow_up_of_a_call_the_fence_didn_t_register_is_refused_before_it_is_sent() {
    let lab = Lab::start().await;
    let log = SharedLog::default();
    let outbox = outbox_of(&lab, &log, true);
    let write =
        AcknowledgeMediaAnswer::new(&CallContext::fresh().unwrap(), &CallLink::parse(UNREGISTERED).unwrap()).unwrap();
    let tried = outbox.try_call(write).await;
    assert!(matches!(tried, CallTried::Denied(_)), "{tried:?}");
    let denials = log.denials();
    assert_eq!(denials.len(), 1, "{denials:#?}");
    assert_eq!(denials[0]["reason"], json!({"partial": "unregistered_call"}));
    let sent = lab.mock.received_requests().await;
    assert!(
        !sent
            .iter()
            .any(|request| request.url.path().contains("MockCallNobodyStarted")),
        "nothing went out"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_whose_answer_doesn_t_read_registers_nothing_and_fails_the_call() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.answer_next("POST", "/epconv", 200, &json!({})).await;
    let call = a.handle.requests().call(test_call()).await.unwrap();
    assert_eq!(a.ended(call).await, End::Failed("start"));
    let permitted = a.log.permitted();
    assert_eq!(
        permitted.len(),
        1,
        "the start only, no leave without a link: {permitted:#?}"
    );
    assert!(lab.mock.calls().is_empty(), "the world never saw it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_acceptance_that_binds_nothing_is_never_acknowledged_and_the_call_leaves() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.unbind_next_acceptance();
    let call = a.handle.requests().call(test_call()).await.unwrap();
    assert_eq!(a.ended(call).await, End::Failed("acknowledge"));
    wait_until(|| lab.sent_by(LAB_A).contains(&"leave".to_owned())).await;
    assert_eq!(
        lab.sent_by(LAB_A),
        ["start", "leave"],
        "the leave on the conversation the start registered; nothing on the acceptance's links"
    );
    assert_eq!(lab.responses_of(LAB_A), 0, "no acknowledgement");
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_media_setup_that_fails_refuses_the_action_and_sends_nothing() {
    let lab = Lab::start().await;
    let a = lab.account(LAB_A, A_TONE).await;
    lab.mock.refuse_next("GET", "/trap/tokens", 500).await;
    let refused = a.handle.requests().call(test_call()).await;
    assert!(
        matches!(refused, Err(EngineError::Call(CallActionError::Media(_)))),
        "{refused:?}"
    );
    assert!(lab.mock.calls().is_empty());
    assert!(a.log.permitted().is_empty());
    // and the next one goes
    let call = a.handle.requests().call(test_call()).await.unwrap();
    let mut a = a;
    a.status(call, &CallStatus::Connected).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_whose_media_fails_keeps_the_invitation_ringing_until_it_is_declined() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let _ = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    lab.mock.refuse_next("GET", "/trap/tokens", 500).await;
    let requests = b.handle.requests();
    let refused = requests.call(CallAction::Answer { call: incoming.call }).await;
    assert!(
        matches!(refused, Err(EngineError::Call(CallActionError::Media(_)))),
        "{refused:?}"
    );
    let quiet = b.events_within(Duration::from_millis(300)).await;
    assert!(quiet.is_empty(), "it still rings: {quiet:#?}");
    requests
        .call(CallAction::Decline { call: incoming.call })
        .await
        .unwrap();
    assert_eq!(b.ended(incoming.call).await, End::Declined);
    assert_eq!(lab.sent_by(LAB_B), ["attach", "progress", "reject"]);
}

/// An `attach` refused with a 401 (the skypetoken renewed) goes once more on the same invitation: the lab's fence keeps
/// it offered, so it rings and is answered (the `accept` needs its invitation still offered).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attach_refused_with_a_401_goes_again_and_its_invitation_is_answered() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    lab.mock.refuse_next("POST", "/attach", 401).await;
    let _ = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
    assert!(b.log.denials().is_empty(), "{:#?}", b.log.denials());
    let attaches = b.log.permitted().iter().filter(|line| line.contains("/attach")).count();
    assert_eq!(
        attaches,
        2,
        "the refused attach and the one sent again: {:#?}",
        b.log.permitted()
    );
    let sent = lab.sent_by(LAB_B);
    assert_eq!(sent.first().map(String::as_str), Some("attach"), "{sent:?}");
    assert!(sent.contains(&"accept".to_owned()), "{sent:?}");
}

/// The far end closing the media with no end of the call delivered after it (seen live: a web client's hang-up
/// closed its media and no `call/end` came): the far end left, and the account's leg leaves too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_far_end_closing_the_media_ends_the_call_as_the_far_end_s_with_a_leave() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let call = a.handle.requests().call(test_call()).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    let _ = lab.mock.close_peer(&lab.last_call(), LAB_A);
    assert_eq!(a.ended(call).await, End::Remote(teams_call::RemoteEnd::default()));
    wait_until(|| lab.sent_by(LAB_A).last().map(String::as_str) == Some("leave")).await;
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_the_fence_denies_never_opens_the_microphone() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let requests = a.handle.requests();
    // a 1:1 chat with someone outside the lab: the media is set up, the start denied
    let call = requests.call(CallAction::Call { chat: AX_CHAT.into() }).await.unwrap();
    assert_eq!(a.ended(call).await, End::Failed("start"));
    assert_eq!(a.log.denials().len(), 1, "{:#?}", a.log.denials());
    assert_eq!(a.microphones(), 0, "the microphone never opened");
    // a start the fence permits opens it, once it was taken
    let call = requests.call(test_call()).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    assert_eq!(a.microphones(), 1);
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_sent_again_after_a_throttle_connects_and_its_acceptance_is_acknowledged() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.refuse_next("POST", "/epconv", 503).await;
    let requests = a.handle.requests();
    let call = requests.call(test_call()).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    let starts = a
        .log
        .permitted()
        .iter()
        .filter(|line| line.contains("\"call\""))
        .count();
    assert_eq!(
        starts,
        2,
        "the throttled start and the one sent again: {:#?}",
        a.log.permitted()
    );
    assert_acceptance_route(&lab, &a.log).await;
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
}

/// The acceptance uses its delivery response, or the documented HTTP fallback when that connection went away.
/// Validate the optional request before removing its one position from the ordered call operations.
async fn assert_acceptance_route(lab: &Lab, log: &SharedLog) {
    let operations = lab.sent_by(LAB_A);
    let acknowledgements: Vec<usize> = operations
        .iter()
        .enumerate()
        .filter_map(|(at, operation)| (operation == "acknowledge").then_some(at))
        .collect();
    assert!(acknowledgements.len() <= 1, "one acknowledgement route: {operations:?}");
    let all = lab.mock.received_requests().await;
    let http: Vec<_> = all
        .iter()
        .filter(|request| {
            request.url.path().contains("/callAcceptance/") && request.url.path().ends_with("/acknowledge")
        })
        .collect();
    assert_eq!(http.len(), acknowledgements.len());
    let permits: Vec<_> = log
        .lines()
        .into_iter()
        .filter(|line| {
            line["kind"] == "call_follow_up" && line["scope"]["call_operation"]["operation"] == "acknowledge"
        })
        .collect();
    assert_eq!(permits.len(), http.len(), "each fallback was checked by the fence");
    if let Some(&at) = acknowledgements.first() {
        assert!(at > 1 && at < operations.len() - 1, "{operations:?}");
        let call = lab.mock.calls().pop().unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../../tests/fixtures/calling/echo/answers.json")).unwrap();
        let expected = fixture["acceptance_callback"]["body"]["callAcceptance"]["links"]["acknowledgement"]
            .as_str()
            .unwrap()
            .replace("<guid:5>", &call.id);
        let url = reqwest::Url::parse(&expected).unwrap();
        assert_eq!(http[0].method.as_str(), "POST");
        assert_eq!(http[0].url.path(), url.path(), "the link belongs to this accepted call");
        assert_eq!(http[0].url.query(), url.query());
        assert_eq!(permits[0]["verdict"], "allow");
        assert_eq!(
            permits[0]["scope"]["call_operation"]["url"],
            teams_fence::log_url(&expected)
        );
        assert_acknowledgement_payload(http[0], &all);
        eprintln!(
            "call {}: acceptance acknowledged once by its admitted HTTP fallback",
            call.id
        );
    } else {
        eprintln!("acceptance acknowledged once by its delivery response");
    }
    let ordered: Vec<_> = operations
        .iter()
        .enumerate()
        .filter(|(at, _)| !acknowledgements.contains(at))
        .map(|(_, operation)| operation.as_str())
        .collect();
    assert_eq!(ordered, ["start", "updateEndpointMetadata", "add", "leave"]);
    assert_eq!(
        lab.responses_of(LAB_A) + http.len(),
        1,
        "one way, with no duplicate acknowledgement"
    );
}

fn assert_acknowledgement_payload(request: &wiremock::Request, requests: &[wiremock::Request]) {
    let start = requests
        .iter()
        .find(|request| {
            request.url.path().ends_with("/epconv")
                && serde_json::from_slice::<serde_json::Value>(&request.body).is_ok()
        })
        .unwrap();
    let started: serde_json::Value = serde_json::from_slice(&start.body).unwrap();
    let acceptance = started["callInvitation"]["links"]["acceptance"].as_str().unwrap();
    let callback = acceptance.strip_suffix("/call/acceptance/").unwrap();
    let agent = format!("{}/", callback.rsplit_once('/').unwrap().0);
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 1);
    let acknowledgement = body["callAcceptanceAcknowledgement"].as_object().unwrap();
    assert_eq!(acknowledgement.len(), 1);
    let links = acknowledgement["links"].as_object().unwrap();
    assert_eq!(links.len(), 7);
    for name in [
        "mediaRenegotiation",
        "transfer",
        "replacement",
        "balanceUpdate",
        "retargetCompletion",
        "controlVideoStreaming",
        "updateMediaDescriptions",
    ] {
        let link = links[name].as_str().unwrap();
        assert!(
            link.starts_with(&agent),
            "the fallback hands out only this call agent's callbacks"
        );
        assert!(link.ends_with(&format!("/call/{name}/")));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_throttled_start_hung_up_during_its_wait_is_never_sent_again() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.refuse_next("POST", "/epconv", 503).await;
    let requests = a.handle.requests();
    let call = requests.call(test_call()).await.unwrap();
    // the start went and was throttled: its next attempt waits a second (no `Retry-After`); the account hangs up
    // meanwhile
    wait_until(|| a.log.permitted().len() == 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    requests.call(CallAction::HangUp { call }).await.unwrap();
    let _ = a.ended(call).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        a.log.permitted().len(),
        1,
        "the start went once: {:#?}",
        a.log.permitted()
    );
    assert!(lab.mock.calls().is_empty(), "no call rang anywhere");
    assert_eq!(a.microphones(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_renegotiation_while_the_first_waits_is_refused_and_the_call_goes_on() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let id = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;
    let first = requests.call(CallAction::Renegotiate { call });
    let second = requests.call(CallAction::Renegotiate { call });
    assert_eq!(first.await.unwrap(), call);
    let refused = second.await;
    assert!(
        matches!(
            refused,
            Err(EngineError::Call(CallActionError::Call(teams_call::CallError::NotNow)))
        ),
        "{refused:?}"
    );
    wait_until(|| lab.sent_by(LAB_B).contains(&"acknowledge".to_owned())).await;
    let before = lab.mock.peer_stats(&id, LAB_B).unwrap().received;
    tokio::time::sleep(TALK).await;
    assert!(
        lab.mock.peer_stats(&id, LAB_B).unwrap().received > before + 50,
        "the media goes on after the first's answer"
    );
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
    let sent = lab.sent_by(LAB_B);
    assert_eq!(sent.iter().filter(|op| *op == "renegotiate").count(), 1, "{sent:?}");
    assert!(b.log.denials().is_empty(), "{:#?}", b.log.denials());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_engine_stopping_hangs_the_call_up_with_a_leave() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let call = a.handle.requests().call(test_call()).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    let log = a.shut_down().await;
    wait_until(|| lab.sent_by(LAB_A).last().map(String::as_str) == Some("leave")).await;
    assert!(log.permitted_any("\"leave\""), "{:#?}", log.permitted());
    assert!(log.denials().is_empty(), "{:#?}", log.denials());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_engine_stopping_while_the_start_is_in_flight_waits_for_it_and_leaves() {
    let lab = Lab::start().await;
    let a = lab.account(LAB_A, A_TONE).await;
    // the action is answered once the start was handed to the outbox: it is in flight as the engine stops
    let _ = a.handle.requests().call(test_call()).await.unwrap();
    let log = a.shut_down().await;
    wait_until(|| lab.sent_by(LAB_A).last().map(String::as_str) == Some("leave")).await;
    assert_eq!(lab.sent_by(LAB_A).first().map(String::as_str), Some("start"));
    assert!(log.denials().is_empty(), "{:#?}", log.denials());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_answered_after_the_shutdown_grace_still_gets_its_leave() {
    let lab = Lab::start().await;
    let a = lab.account(LAB_A, A_TONE).await;
    // the start's answer comes past `SHUTDOWN_GRACE`, within the request's timeout
    lab.mock
        .delay_next_call_op("epconv", teams_core::calling::SHUTDOWN_GRACE + Duration::from_secs(2));
    let _ = a.handle.requests().call(test_call()).await.unwrap();
    let log = a.shut_down().await;
    wait_until(|| lab.sent_by(LAB_A).last().map(String::as_str) == Some("leave")).await;
    assert_eq!(lab.sent_by(LAB_A).first().map(String::as_str), Some("start"));
    assert!(log.denials().is_empty(), "{:#?}", log.denials());
}

/// a's token in a JWT's shape (made-up claims, no signature checked): its audience and the mock's tenant.
const A_TOKEN_WITH_TENANT: &str = "eyJhbGciOiJub25lIn0.\
    eyJhdWQiOiJodHRwczovL2ljMy50ZWFtcy5vZmZpY2UuY29tIiwidGlkIjoiMDAwMDAwMDAtMDAwMC00MDAwLTgwMDAtMDAwMDAwMDAwMDAxIn0.\
    c2lnbmF0dXJl";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_call_of_an_engine_without_the_tenant_reads_it_from_the_account_s_token_first() {
    let lab = Lab::start().await;
    assert!(lab.mock.set_token(LAB_A, A_TOKEN_WITH_TENANT));
    let mut a = lab.account_without_tenant(LAB_A, A_TONE).await;
    let requests = a.handle.requests();
    let call = requests
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    wait_until(|| lab.sent_by(LAB_A).contains(&"add".to_owned())).await;
    // the `add` names the tenant read from the token (its `tid`, the mock's tenant)
    let added = lab.mock.calls().last().expect("a call").add_tenants.clone();
    assert_eq!(added, [TENANT]);
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert!(a.log.denials().is_empty(), "{:#?}", a.log.denials());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_call_whose_tenant_can_t_be_read_is_refused_and_sends_nothing() {
    let lab = Lab::start().await;
    // the mock's own tokens are opaque: no tenant in them
    let a = lab.account_without_tenant(LAB_A, A_TONE).await;
    let refused = a
        .handle
        .requests()
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await;
    assert!(
        matches!(refused, Err(EngineError::Call(CallActionError::Media(ref why))) if why.contains("tenant")),
        "{refused:?}"
    );
    assert!(lab.mock.calls().is_empty(), "no start went");
    assert!(a.log.permitted().is_empty(), "{:#?}", a.log.permitted());
    assert_eq!(a.microphones(), 0);
}
