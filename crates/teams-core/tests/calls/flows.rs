//! A call connects and a tone crosses both ways: the test call against the media controller's peer, a → b native on
//! both ends, the invitation of a caller the mock plays (with a renegotiation either way), a running group call
//! joined; mute (asked while its media is set up too); a call outliving its calling socket's reconnect.

use std::time::Duration;

use crate::support::{A_TONE, B_TONE, Lab, PEER_TONE, TALK, heard, heard_before, wait_until};
use teams_core::{CallAction, CallStatus, End, Event, RegistrationReport};
use teams_mock::{AB_CHAT, ABC_CHAT, ECHO, LAB_A, LAB_B, LAB_C, TIMED_OUT};
use teams_proto::Mri;

/// The far tone is heard at least this loud in a recording (a tone of 0.5 through Opus and the jitter buffer).
const HEARD: f32 = 0.25;
/// A tone not sent is heard at most this loud.
const SILENT: f32 = 0.03;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_test_call_connects_its_tone_crosses_both_ways_and_its_operations_are_ordered() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let requests = a.handle.requests();
    let echo = CallAction::TestCall { echo: Mri::new(ECHO) };
    let call = requests.call(echo.clone()).await.unwrap();
    a.status(call, &CallStatus::Connecting).await;
    a.status(call, &CallStatus::Connected).await;
    assert!(
        matches!(
            requests.call(echo).await,
            Err(teams_core::engine::EngineError::Call(teams_core::CallActionError::Busy))
        ),
        "one call at a time"
    );
    tokio::time::sleep(TALK).await;
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);

    let id = lab.last_call();
    assert_eq!(
        lab.sent_by(LAB_A),
        ["start", "updateEndpointMetadata", "add", "leave"],
        "the start, the follow-ups on its registered call, the leave"
    );
    assert_eq!(
        lab.responses_of(LAB_A),
        1,
        "the acceptance's acknowledgement, as its delivery response"
    );
    // the media crossed both ways: the peer's tone in a's recording, a's in the peer's
    tokio::time::sleep(Duration::from_millis(300)).await;
    let peer = lab.mock.close_peer(&id, LAB_A).expect("the media controller's peer");
    assert!(peer.received > 50, "{peer:?}");
    let recording = lab.mock.peer_recording(&id, LAB_A).unwrap();
    let a_heard = heard(&a.recording, PEER_TONE, TALK);
    let peer_heard = heard(&recording, A_TONE, TALK);
    eprintln!("a heard the peer at {a_heard:.3}, the peer heard a at {peer_heard:.3}");
    assert!(a_heard > HEARD && peer_heard > HEARD);
    assert!(heard(&a.recording, A_TONE, TALK) < SILENT, "a doesn't hear itself");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_calls_b_b_answers_by_its_action_and_each_hears_the_other() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call = a
        .handle
        .requests()
        .call(CallAction::Call { chat: AB_CHAT.into() })
        .await
        .unwrap();
    a.status(call, &CallStatus::Ringing).await;
    let incoming = b.incoming().await;
    assert_eq!(incoming.caller.as_str(), LAB_A);
    assert_eq!(incoming.group_chat, None);
    // nothing answers it but b's action
    let quiet = b.events_within(Duration::from_millis(500)).await;
    assert!(
        !quiet.iter().any(|event| matches!(event, Event::Call { .. })),
        "{quiet:#?}"
    );
    assert!(!lab.sent_by(LAB_B).contains(&"accept".to_owned()));
    let answered = b
        .handle
        .requests()
        .call(CallAction::Answer { call: incoming.call })
        .await
        .unwrap();
    assert_eq!(answered, incoming.call);
    b.status(incoming.call, &CallStatus::Connected).await;
    a.status(call, &CallStatus::Connected).await;
    // each sees the other, on the 1:1 call's one line, from the roster that lists no media streams
    let shows = |who: &'static str| {
        move |view: &teams_core::CallView| {
            view.participants.len() == 1 && view.participants[0].mri == who && view.participants[0].line == Some(0)
        }
    };
    a.view(call, shows(LAB_B)).await;
    b.view(incoming.call, shows(LAB_A)).await;
    tokio::time::sleep(TALK).await;
    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert!(matches!(b.ended(incoming.call).await, End::Remote(_)));

    assert_eq!(
        lab.sent_by(LAB_B),
        [
            "attach",
            "progress",
            "updateEndpointState",
            "accept",
            "updateEndpointMetadata"
        ]
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (a_heard, b_heard) = (heard(&a.recording, B_TONE, TALK), heard(&b.recording, A_TONE, TALK));
    eprintln!("a heard b at {a_heard:.3}, b heard a at {b_heard:.3}");
    assert!(a_heard > HEARD && b_heard > HEARD);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_b_declines_goes_to_b_s_voicemail_and_a_is_told_so() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let requests = a.handle.requests();
    let call = requests.call(CallAction::Call { chat: AB_CHAT.into() }).await.unwrap();
    a.status(call, &CallStatus::Ringing).await;
    let incoming = b.incoming().await;
    b.handle
        .requests()
        .call(CallAction::Decline { call: incoming.call })
        .await
        .unwrap();
    assert_eq!(b.ended(incoming.call).await, End::Declined);
    // as captured: the call controller forwards a's call to b's voicemail, which takes it
    a.next(|event| matches!(event, Event::Voicemail { call: of } if *of == call).then_some(()))
        .await;
    // it runs on until a hangs up
    let quiet = a.events_within(Duration::from_secs(2)).await;
    assert!(
        !quiet.iter().any(|event| matches!(
            event,
            Event::Call {
                status: CallStatus::Ended(_),
                ..
            }
        )),
        "{quiet:#?}"
    );
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invitation_s_offer_is_answered_and_a_renegotiation_goes_either_way() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    // a caller the mock plays: its peer offers in the invitation
    let id = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;

    // b's own renegotiation: the peer answers it, b applies the answer and acknowledges it
    assert_eq!(requests.call(CallAction::Renegotiate { call }).await.unwrap(), call);
    wait_until(|| lab.sent_by(LAB_B).contains(&"acknowledge".to_owned())).await;
    // the peer's renegotiation: b answers it
    assert!(lab.mock.renegotiate(&id, LAB_B));
    wait_until(|| lab.sent_by(LAB_B).contains(&"answer".to_owned())).await;
    tokio::time::sleep(TALK).await;
    let peer_before = lab.mock.peer_stats(&id, LAB_B).unwrap().received;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        lab.mock.peer_stats(&id, LAB_B).unwrap().received > peer_before,
        "the media goes on"
    );
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);

    let sent = lab.sent_by(LAB_B);
    assert_eq!(
        sent,
        [
            "attach",
            "progress",
            "updateEndpointState",
            "accept",
            "updateEndpointMetadata",
            "renegotiate",
            "acknowledge",
            "answer",
            "leave"
        ]
    );
    let _ = lab.mock.close_peer(&id, LAB_B);
    let peer_heard = heard(&lab.mock.peer_recording(&id, LAB_B).unwrap(), B_TONE, TALK);
    let b_heard = heard(&b.recording, PEER_TONE, TALK);
    eprintln!("after both renegotiations b heard the peer at {b_heard:.3}, the peer heard b at {peer_heard:.3}");
    assert!(b_heard > HEARD && peer_heard > HEARD);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_group_call_is_joined_with_media_and_carries_it() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let id = lab.mock.ring_group(LAB_A, ABC_CHAT, &[LAB_B]);
    let incoming = b.incoming().await;
    assert_eq!(incoming.group_chat.as_deref(), Some(ABC_CHAT));
    assert!(lab.mock.end_ring(&id, 487, TIMED_OUT));
    b.next(|event| matches!(event, Event::RingEnded { .. }).then_some(()))
        .await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Join { chat: ABC_CHAT.into() }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;
    tokio::time::sleep(TALK).await;
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
    assert!(lab.sent_by(LAB_B).contains(&"join".to_owned()));
    let _ = lab.mock.close_peer(&id, LAB_B);
    let peer_heard = heard(&lab.mock.peer_recording(&id, LAB_B).unwrap(), B_TONE, TALK);
    let b_heard = heard(&b.recording, PEER_TONE, TALK);
    eprintln!("joined: b heard the mixer's peer at {b_heard:.3}, the peer heard b at {peer_heard:.3}");
    assert!(b_heard > HEARD && peer_heard > HEARD);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_muted_microphone_sends_silence_and_the_call_goes_on() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let requests = a.handle.requests();
    let call = requests
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    tokio::time::sleep(TALK).await;
    requests.call(CallAction::Mute { call, muted: true }).await.unwrap();
    tokio::time::sleep(TALK).await;
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert!(
        matches!(
            requests.call(CallAction::Mute { call, muted: false }).await,
            Err(teams_core::engine::EngineError::Call(
                teams_core::CallActionError::NoMedia
            ))
        ),
        "no media runs once it ended"
    );
    let id = lab.last_call();
    let _ = lab.mock.close_peer(&id, LAB_A);
    let recording = lab.mock.peer_recording(&id, LAB_A).unwrap();
    let muted = heard(&recording, A_TONE, Duration::from_secs(1));
    let before = heard_before(
        &recording,
        A_TONE,
        Duration::from_secs(1),
        TALK + Duration::from_millis(500),
    );
    eprintln!("the peer heard a at {before:.3}, then muted at {muted:.4}");
    assert!(before > HEARD && muted < SILENT);
    assert_eq!(lab.sent_by(LAB_A).last().map(String::as_str), Some("leave"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mute_asked_while_its_media_is_set_up_holds_from_its_first_sample() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    // the relay's credentials come late: the media is still being set up when the mute is asked
    lab.mock
        .answer_next_after("GET", "/trap/tokens", Duration::from_secs(2))
        .await;
    let requests = a.handle.requests();
    let start = requests.call(CallAction::TestCall { echo: Mri::new(ECHO) });
    let told = a
        .next(|event| match event {
            Event::Call {
                call,
                status: CallStatus::Placing,
            } => Some(*call),
            _ => None,
        })
        .await;
    assert_eq!(
        requests
            .call(CallAction::Mute {
                call: told,
                muted: true
            })
            .await
            .unwrap(),
        told,
        "held for its media"
    );
    assert_eq!(start.await.unwrap(), told);
    a.status(told, &CallStatus::Connected).await;
    tokio::time::sleep(TALK).await;
    // unmuted, the tone goes: the recording heard the call all along
    requests
        .call(CallAction::Mute {
            call: told,
            muted: false,
        })
        .await
        .unwrap();
    tokio::time::sleep(TALK).await;
    requests.call(CallAction::HangUp { call: told }).await.unwrap();
    assert_eq!(a.ended(told).await, End::HungUp);
    let id = lab.last_call();
    let _ = lab.mock.close_peer(&id, LAB_A);
    let recording = lab.mock.peer_recording(&id, LAB_A).unwrap();
    let muted = heard_before(
        &recording,
        A_TONE,
        Duration::from_secs(1),
        TALK + Duration::from_millis(500),
    );
    let unmuted = heard(&recording, A_TONE, Duration::from_secs(1));
    eprintln!("the peer heard a at {muted:.4} while muted, then at {unmuted:.3}");
    assert!(muted < SILENT && unmuted > HEARD);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_outlives_its_calling_socket_s_reconnect_and_keeps_its_media() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let requests = a.handle.requests();
    let call = requests
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    let id = lab.last_call();

    lab.trouter.drop_sockets(LAB_A);
    // the calling socket comes back on its endpoint and registers again, the call untouched
    a.next(|event| match event {
        Event::CallingRegistration(RegistrationReport::Registered) => Some(()),
        Event::Call { status, .. } if *status != CallStatus::Connected => panic!("the call moved: {status:?}"),
        _ => None,
    })
    .await;
    let before = lab.mock.peer_stats(&id, LAB_A).unwrap().received;
    tokio::time::sleep(TALK).await;
    let after = lab.mock.peer_stats(&id, LAB_A).unwrap().received;
    assert!(after > before + 50, "the media went on: {before} → {after}");
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert_eq!(
        lab.sent_by(LAB_A),
        ["start", "updateEndpointMetadata", "add", "leave"],
        "the leave on the links the call had before"
    );
    let _ = lab.mock.close_peer(&id, LAB_A);
    assert!(heard(&a.recording, PEER_TONE, TALK) > HEARD);
}
