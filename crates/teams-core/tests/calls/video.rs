//! A 1:1 call's video (M3 Task 18), on the mock and its loopback peer (H.264 on OpenH264): the camera off
//! when the call starts and turned on only by the account's action, as a renegotiation of the call's media the peer
//! answers; the peer's video shown once its renegotiation offers it; the camera turned off stops what is sent; the test
//! call's camera as a 1:1 call's, the media controller's peer answering. A group call's video is `group`'s.

use std::time::Duration;

use crate::support::{A_TONE, B_TONE, Lab, wait_until};
use teams_core::{CallAction, CallStatus, End};
use teams_mock::{AB_CHAT, ECHO, LAB_A, LAB_B, LAB_C};
use teams_proto::Mri;

/// Frames a side must have decoded of the other's test pattern.
const FRAMES: u64 = 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_camera_goes_on_by_the_account_s_action_and_both_ways_carry_video() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let id = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        b.video.preview.counts().frames,
        0,
        "the camera is off when a call starts"
    );
    assert_eq!(lab.mock.peer_stats(&id, LAB_B).unwrap().video.received, 0);

    // b turns its camera on: a renegotiation the peer answers sending its own, b's frames reach it
    assert_eq!(
        requests.call(CallAction::Camera { call, on: true }).await.unwrap(),
        call
    );
    wait_until(|| lab.sent_by(LAB_B).contains(&"acknowledge".to_owned())).await;
    wait_until(|| lab.mock.peer_stats(&id, LAB_B).unwrap().video.shown >= FRAMES).await;
    wait_until(|| b.video.remote.counts().indexed >= FRAMES).await;
    assert!(b.video.preview.counts().frames >= FRAMES, "b's self view");

    // and off: nothing more of b's reaches the peer
    assert_eq!(
        requests.call(CallAction::Camera { call, on: false }).await.unwrap(),
        call
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    let shown = lab.mock.peer_stats(&id, LAB_B).unwrap().video.shown;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        lab.mock.peer_stats(&id, LAB_B).unwrap().video.shown,
        shown,
        "b's camera stopped"
    );

    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
    let sent = lab.sent_by(LAB_B);
    assert_eq!(
        sent.iter().filter(|op| *op == "renegotiate").count(),
        2,
        "on, then off: {sent:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_test_call_s_camera_goes_on_as_a_one_to_one_call_s_and_shows_the_self_view() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let requests = a.handle.requests();
    let call = requests
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    let id = lab.last_call();
    assert_eq!(
        a.video.preview.counts().frames,
        0,
        "the camera is off when the test call starts"
    );

    // a turns its camera on: a renegotiation the media controller's peer answers, a's frames reach it
    assert_eq!(
        requests.call(CallAction::Camera { call, on: true }).await.unwrap(),
        call
    );
    wait_until(|| lab.mock.peer_stats(&id, LAB_A).unwrap().video.shown >= FRAMES).await;
    assert!(a.video.preview.counts().frames >= FRAMES, "a's self view");

    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    let sent = lab.sent_by(LAB_A);
    assert_eq!(
        sent,
        [
            "start",
            "updateEndpointMetadata",
            "add",
            "renegotiate",
            "acknowledge",
            "leave"
        ],
        "the test call's writes, its camera's renegotiation among them"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_far_end_s_video_is_shown_while_the_camera_stays_off() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let id = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;

    // the peer turns its camera on: b answers it receiving, and shows its frames
    assert!(lab.mock.renegotiate_video(&id, LAB_B));
    wait_until(|| lab.sent_by(LAB_B).contains(&"answer".to_owned())).await;
    wait_until(|| b.video.remote.counts().indexed >= FRAMES).await;
    let counts = b.video.remote.counts();
    // a moving picture (the peer's rate control may skip frames at a low estimate: gaps, never a frozen picture)
    assert!(
        counts.changed + 1 >= counts.frames,
        "{counts:?}; the peer {:?}",
        lab.mock.peer_stats(&id, LAB_B).unwrap().video
    );
    assert_eq!(b.video.preview.counts().frames, 0, "b's camera stays off");
    assert_eq!(
        lab.mock.peer_stats(&id, LAB_B).unwrap().video.received,
        0,
        "b sends no video"
    );

    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_ends_turn_their_cameras_on_and_each_shows_the_other_s() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call = a
        .handle
        .requests()
        .call(CallAction::Call { chat: AB_CHAT.into() })
        .await
        .unwrap();
    let incoming = b.incoming().await;
    let answered = b
        .handle
        .requests()
        .call(CallAction::Answer { call: incoming.call })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    b.status(answered, &CallStatus::Connected).await;

    // a's camera: its renegotiation reaches b, whose answer (b's camera off: receiving) reaches a
    a.handle
        .requests()
        .call(CallAction::Camera { call, on: true })
        .await
        .unwrap();
    wait_until(|| b.video.remote.counts().indexed >= FRAMES).await;
    assert_eq!(a.video.remote.counts().frames, 0, "b's camera is off");
    // b's camera too: both ways
    b.handle
        .requests()
        .call(CallAction::Camera {
            call: answered,
            on: true,
        })
        .await
        .unwrap();
    wait_until(|| a.video.remote.counts().indexed >= FRAMES).await;

    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    let sent_by = |mri| lab.sent_by(mri);
    for (who, sent) in [("a", sent_by(LAB_A)), ("b", sent_by(LAB_B))] {
        assert!(
            sent.contains(&"renegotiate".to_owned()) && sent.contains(&"answer".to_owned()),
            "{who}: {sent:?}"
        );
    }
}
