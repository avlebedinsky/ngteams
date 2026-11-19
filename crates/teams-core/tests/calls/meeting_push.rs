//! A meeting's start the calling service pushes (`evt` 128 at the calling endpoint's root, `teams_call::meeting_push`)
//! is told as [`Event::MeetingStarted`]; nothing rings for it, and the call tracker leaves it.

use std::time::Duration;

use base64::Engine as _;
use serde_json::json;
use teams_core::{Event, MeetingStartKind};
use teams_mock::{LAB_A, LAB_B};

use crate::support::{B_TONE, Lab};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pushed_meeting_start_is_told_and_rings_nothing() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let payload = json!({
        "_synthetic": true,
        "notificationType": "MeetingStart",
        "callId": "made-up-call",
        "threadId": "19:meeting_MadeUpPushed@thread.v2",
        "messageId": "0",
        "subject": "Made-up planning",
        "groupCallInitiator": LAB_A,
        "groupCallInitiatorDisplayName": "Lab A",
        "meetingInfo": {"tenantId": "made-up-tenant", "organizerId": "made-up-organizer"}
    });
    let gp = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
    let _ = lab.trouter.deliver_call(LAB_B, "", &json!({"evt": 128, "gp": gp}));
    let start = b
        .next(|event| match event {
            Event::MeetingStarted(start) => Some(start.clone()),
            _ => None,
        })
        .await;
    assert_eq!(start.kind, MeetingStartKind::Meeting);
    assert_eq!(start.thread, "19:meeting_MadeUpPushed@thread.v2");
    assert_eq!(start.call_id, "made-up-call");
    assert_eq!(start.starter.as_deref(), Some(LAB_A));
    assert_eq!(start.subject.as_deref(), Some("Made-up planning"));
    let later = b.events_within(Duration::from_millis(500)).await;
    assert!(
        !later.iter().any(|event| matches!(event, Event::IncomingCall(_))),
        "{later:?}"
    );
}
