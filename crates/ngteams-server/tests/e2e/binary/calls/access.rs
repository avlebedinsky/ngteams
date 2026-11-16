//! Who a call is for: a third account touches nothing of it, a callee already in a call isn't disturbed, and the
//! calls this server doesn't serve (a meeting, the test call) are refused without harm. Group calls are served
//! (`group.rs`).

use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};
use teams_core::engine::{Action, Event};
use teams_core::{CallAction, CallStatus, End};
use teams_proto::chat::ChatReads;
use teams_proto::{MeetingInfo, Mri};
use teams_store::OutboxState;

use super::support::{C_TONE, Line, Phone, Rig, TALK, Turn, raw};
use super::{Talk, connect, hang_up, pair};
use crate::support::backends;

const PROXY: &str = "/flightproxy/api/v2";

/// Every request of a call's, by its conversation and call ids: its own links and others of their shape.
fn requests_of(conversation: &str, call: &str) -> Vec<(Method, String)> {
    let conv = format!("{PROXY}/ep/conv.own/conv/{conversation}");
    let cc = format!("{PROXY}/ep/cc.own/cc/v1");
    let mut requests = vec![(Method::POST, conv.clone()), (Method::DELETE, conv.clone())];
    for op in [
        "leave",
        "updateEndpointState",
        "updateEndpointMetadata",
        "updateParticipantProperties",
        "notificationLinks",
    ] {
        requests.push((Method::POST, format!("{conv}/{op}")));
    }
    requests.push((Method::POST, format!("{cc}/forked/{call}/1/i1/1/attach")));
    requests.push((Method::POST, format!("{cc}/incoming/{call}/1/t/1/accept")));
    requests.push((Method::DELETE, format!("{cc}/incoming/{call}/1/t/1/reject")));
    requests.push((Method::POST, format!("{cc}/active/{call}/1/a1/1")));
    for op in ["renegotiate", "hold", "resume", "updateMediaDescriptions", "leave"] {
        requests.push((Method::POST, format!("{cc}/active/{call}/1/a1/1/{op}")));
    }
    requests
}

/// A start of a call as `from` to `to`, its callbacks at `callbacks`.
fn start_body(from: &str, to: &str, callbacks: &str) -> Value {
    let link = |name: &str| format!("{callbacks}{name}/");
    json!({
        "conversationRequest": {
            "roster": {"type": "Delta", "rosterUpdate": link("rosterUpdate")},
            "links": {"conversationEnd": link("conversationEnd"), "conversationUpdate": link("conversationUpdate")},
        },
        "participants": {
            "from": {"id": from, "displayName": "Ada", "endpointId": "00000000-0000-4000-8000-0000000000aa",
                "participantId": "00000000-0000-4000-8000-0000000000ab", "languageId": "en-us"},
            "to": [{"id": to, "participantId": "00000000-0000-4000-8000-0000000000bb"}],
        },
        "groupChat": null,
        "callInvitation": {
            "callModalities": ["Audio"],
            "links": {"progress": link("progress"), "acceptance": link("acceptance"), "end": link("end"),
                "mediaRenegotiation": link("mediaRenegotiation")},
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        },
    })
}

/// Neither call ended and nobody was rung since the marks.
fn undisturbed(phone: &Phone, mark: usize) -> bool {
    !phone.log[mark..].iter().any(|event| {
        matches!(
            event,
            Event::IncomingCall(_)
                | Event::RingEnded { .. }
                | Event::Call {
                    status: CallStatus::Ended(_),
                    ..
                }
        )
    })
}

/// c, by the ids of the call of a and b, through every node: each request is answered as if there were no call.
async fn outsider_requests(rig: &Rig, c: &Phone, a_mri: &str, b_mri: &str) {
    let calls = rig.open_calls().await;
    let [call] = calls.as_slice() else {
        panic!("one call: {calls:?}");
    };
    let token = c.skypetoken().await;
    let c_endpoint = rig.calling_endpoint(c).await;
    for node in [&rig.first, rig.other()] {
        for (method, path) in requests_of(&call.conversation, &call.id) {
            let (status, body) = raw(node, &token, method.clone(), &path, &json!({})).await;
            assert_eq!(status, 404, "{method} {path}: an outsider learns nothing of the call");
            assert!(!body.to_string().contains(&call.id), "{method} {path}: {body}");
        }
        // c can't start a call in a's name (its callbacks on c's own endpoint, so only the name refuses it), nor
        // one whose deliveries go to someone else's socket
        let own = format!("{}/trouter/v4/f/{c_endpoint}/callAgent/x/", rig.origin);
        let elsewhere = format!("{}/trouter/v4/f/SomeoneElsesEndpoint00/callAgent/x/", rig.origin);
        let epconv = format!("{PROXY}/epconv");
        let (status, _) = raw(node, &token, Method::POST, &epconv, &start_body(a_mri, b_mri, &own)).await;
        assert_eq!(status, 403, "a start in another account's name");
        let (status, _) = raw(
            node,
            &token,
            Method::POST,
            &epconv,
            &start_body(&c.mri, b_mri, &elsewhere),
        )
        .await;
        assert_eq!(status, 403, "callbacks on an endpoint that isn't c's");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_third_account_reads_and_disturbs_nothing_of_a_call() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "outsider", 30, Turn::Coturn).await else {
            return;
        };
        let (mut a, mut b, chat) = pair(&rig).await;
        let mut c = rig.phone(&rig.first, "c@own.test", "Cora", C_TONE).await;
        let talk = connect(&mut a, &mut b, &chat).await;
        let (from, to, outside) = (a.mark(), b.mark(), c.mark());
        outsider_requests(&rig, &c, &a.mri, &b.mri).await;
        // the chat of a and b, with its call lines, isn't c's to read
        let read = ChatReads::new(&c.client()).messages(&chat, 10).await;
        assert!(read.is_err(), "c read the chat of a and b");
        assert!(
            c.quiet(outside, Duration::from_secs(1), |event| !undisturbed_event(event))
                .await
        );
        tokio::time::sleep(TALK).await;
        assert!(undisturbed(&a, from) && undisturbed(&b, to), "the call was disturbed");
        hang_up(&mut b, talk.answered, &mut a, talk.call).await;
        super::heard_each_other(&a, super::support::A_TONE, &b, super::support::B_TONE).await;
        rig.no_open_calls().await;
        for phone in [&mut a, &mut b, &mut c] {
            phone.close().await;
        }
        rig.stop().await;
    }
}

fn undisturbed_event(event: &Event) -> bool {
    !matches!(
        event,
        Event::IncomingCall(_) | Event::Call { .. } | Event::RingEnded { .. }
    )
}

/// c calls b, who is in a call: how c's call ended.
async fn call_the_busy(c: &mut Phone, chat: &str, talk: Talk, a: &mut Phone, b: &mut Phone) -> End {
    let (from, to, outside) = (a.mark(), b.mark(), c.mark());
    let call = c.act(CallAction::Call { chat: chat.to_owned() }).await;
    let end = c.ended(outside, call).await;
    assert!(
        b.quiet(to, Duration::from_millis(500), |event| matches!(
            event,
            Event::IncomingCall(_)
        ))
        .await,
        "a second call rang at b while its call is up"
    );
    assert!(
        undisturbed(a, from) && undisturbed(b, to),
        "the call of a and b was disturbed"
    );
    let _ = talk;
    end
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_to_someone_already_in_a_call_rings_out_and_disturbs_nothing() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "busy", 4, Turn::Coturn).await else {
            return;
        };
        let (mut a, mut b, chat) = pair(&rig).await;
        let mut c = rig.phone(&rig.first, "c@own.test", "Cora", C_TONE).await;
        let with_b = c.chat_with(&b.mri).await;
        b.sync().await;
        let talk = connect(&mut a, &mut b, &chat).await;
        let end = call_the_busy(&mut c, &with_b, talk, &mut a, &mut b).await;
        eprintln!("a call to someone in a call: {end:?}");
        assert!(matches!(end, End::Remote(_)), "{end:?}");
        tokio::time::sleep(TALK).await;
        hang_up(&mut a, talk.call, &mut b, talk.answered).await;
        super::heard_each_other(&a, super::support::A_TONE, &b, super::support::B_TONE).await;
        // b sees the call it missed in its chat with c
        let caller = c.mri.clone();
        b.shows_lines(
            &with_b,
            &[&|line| *line == Line::Started, &|line| line.missed_from(&caller)],
        )
        .await;
        rig.no_open_calls().await;
        for phone in [&mut a, &mut b, &mut c] {
            phone.close().await;
        }
        rig.stop().await;
    }
}

/// a presses a call this server doesn't serve: it fails, and nobody rings.
async fn refused(a: &mut Phone, others: &mut [&mut Phone], what: &str, action: CallAction) {
    let from = a.mark();
    let marks: Vec<usize> = others.iter().map(|phone| phone.mark()).collect();
    match a.press(action).await {
        Err(error) => eprintln!("{what}: the client refused it: {error}"),
        Ok(call) => {
            let end = a.ended(from, call).await;
            eprintln!("{what}: {end:?}");
            assert!(matches!(end, End::Failed(_) | End::Remote(_)), "{what}: {end:?}");
        }
    }
    for (phone, mark) in others.iter_mut().zip(marks) {
        let rang = |event: &Event| matches!(event, Event::IncomingCall(_));
        assert!(phone.quiet(mark, Duration::from_secs(1), rang).await, "{what} rang");
    }
}

/// The starts of the kinds this server doesn't serve, as a's own, through every node: each is answered 501 (a group
/// call in a chat that doesn't exist 404) and leaves no call behind.
async fn not_served(rig: &Rig, a: &Phone, b_mri: &str, c_mri: &str) {
    let token = a.skypetoken().await;
    let callbacks = format!("{}/trouter/v4/f/AnyEndpoint000000000000/callAgent/x/", rig.origin);
    let one_to_one = start_body(&a.mri, b_mri, &callbacks);
    let mut group = one_to_one.clone();
    group["groupChat"] = json!({"threadId": "19:00000000000000000000000000000000@thread.v2", "messageId": "0"});
    let mut two = one_to_one.clone();
    two["participants"]["to"] = json!([
        {"id": b_mri, "participantId": "00000000-0000-4000-8000-0000000000bb"},
        {"id": c_mri, "participantId": "00000000-0000-4000-8000-0000000000cc"},
    ]);
    let mut meeting = one_to_one.clone();
    meeting["meetingInfo"] = json!({"organizerId": a.oid(), "tenantId": "00000000-0000-4000-8000-000000000001"});
    let mut echo = one_to_one.clone();
    echo["conversationRequest"]["scenario"] = json!("TestCall");
    echo["participants"]["to"] = json!([]);
    let epconv = format!("{PROXY}/epconv");
    for node in [&rig.first, rig.other()] {
        for (what, body, refusal) in [
            ("a group call in a chat that doesn't exist", &group, 404),
            ("two callees", &two, 501),
            ("a meeting", &meeting, 501),
            ("the test call", &echo, 501),
        ] {
            let (status, _) = raw(node, &token, Method::POST, &epconv, body).await;
            assert_eq!(status, refusal, "{what}");
        }
    }
    assert!(rig.open_calls().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_meeting_and_the_test_call_are_refused_and_the_next_call_works() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "refused", 30, Turn::Coturn).await else {
            return;
        };
        let (mut a, mut b, chat) = pair(&rig).await;
        let mut c = rig.phone(&rig.first, "c@own.test", "Cora", C_TONE).await;
        let create = Action::CreateChat {
            others: vec![Mri::new(b.mri.clone()), Mri::new(c.mri.clone())],
            topic: Some("Trio".into()),
        };
        let row = a.engine().write(create).await.unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        let group = row.message_id.unwrap();
        a.sync().await;

        let meeting = CallAction::JoinMeeting {
            chat: group.clone(),
            meeting: MeetingInfo {
                organizer_id: a.oid().to_owned(),
                tenant_id: "00000000-0000-4000-8000-000000000001".into(),
            },
        };
        refused(&mut a, &mut [&mut b, &mut c], "a meeting", meeting).await;
        let echo = CallAction::TestCall {
            echo: Mri::new("8:echo123"),
        };
        refused(&mut a, &mut [&mut b, &mut c], "the test call", echo).await;
        assert!(rig.open_calls().await.is_empty());
        let (b_mri, c_mri) = (b.mri.clone(), c.mri.clone());
        not_served(&rig, &a, &b_mri, &c_mri).await;

        let talk = connect(&mut a, &mut b, &chat).await;
        tokio::time::sleep(TALK).await;
        hang_up(&mut a, talk.call, &mut b, talk.answered).await;
        super::heard_each_other(&a, super::support::A_TONE, &b, super::support::B_TONE).await;
        rig.no_open_calls().await;
        for phone in [&mut a, &mut b, &mut c] {
            phone.close().await;
        }
        rig.stop().await;
    }
}
