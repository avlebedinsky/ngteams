//! A running call's health, as the call window's Call health pane reads it: the test call's media measured by
//! libwebrtc (its audio both ways, the peer's video received, the relayed connection's round trip), its rates from
//! the second read on; nothing sent for it; none once the call ended.

use std::time::Duration;

use crate::support::{A_TONE, Lab, TALK};
use teams_core::{CallAction, CallStatus, End};
use teams_mock::{ECHO, LAB_A};
use teams_proto::Mri;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_call_s_health_is_its_media_s_own_statistics() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let requests = a.handle.requests();
    let call = requests
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    tokio::time::sleep(TALK).await;
    let sent_before = lab.sent_by(LAB_A);

    let first = requests
        .call_health(call)
        .await
        .unwrap()
        .expect("the call's media runs");
    let audio = &first.audio;
    assert!(audio.sent.as_ref().is_some_and(|sent| sent.packets > 0), "{first:?}");
    assert!(
        audio.received.as_ref().is_some_and(|received| received.packets > 0),
        "{first:?}"
    );
    assert!(
        audio.sent.as_ref().is_some_and(|sent| sent.bitrate_kbps.is_none()),
        "no rate from a first read"
    );
    // the media controller's peer sends its test video, received on the camera's line; nothing is shared
    assert!(first.video.sent.is_none() && first.sharing.is_empty(), "{first:?}");

    tokio::time::sleep(Duration::from_secs(1)).await;
    let second = requests
        .call_health(call)
        .await
        .unwrap()
        .expect("the call's media runs");
    let sent = second.audio.sent.clone().unwrap();
    assert!(sent.bitrate_kbps.is_some_and(|kbps| kbps > 0.0), "{second:?}");
    assert!(
        second
            .audio
            .received
            .as_ref()
            .is_some_and(|received| received.packets > audio.received.as_ref().map_or(0, |before| before.packets)),
        "{second:?}"
    );
    assert!(second.network.round_trip_ms.is_some(), "{second:?}");
    assert_eq!(lab.sent_by(LAB_A), sent_before, "reading the health sends nothing");

    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert_eq!(requests.call_health(call).await.unwrap(), None, "none once it ended");
    let _ = lab.mock.close_peer(&lab.last_call(), LAB_A);
}
