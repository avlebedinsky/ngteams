//! Offline pre-join decisions and capture holds.

use serde_json::json;

use super::*;
use crate::scenario::ab_chat_full::tests::{EventsOut, broadcast, page_with};

#[tokio::test]
async fn prejoin_keeps_media_and_entry_held_but_requires_a_successful_subscription() {
    for status in [200, 503] {
        let temp = tempfile::tempdir().unwrap();
        let mut page = page_with(false, false, move |command, events| {
            let expression = command["params"]["expression"].as_str().unwrap_or("");
            if expression.contains("e.scrollIntoView") && expression.contains("Join now") {
                send_prejoin_requests(events, status);
            }
            if command["method"] == "Fetch.failRequest" {
                return Some(Ok(json!({})));
            }
            None
        })
        .await;
        let mut session = page.session(temp.path(), true);
        hold_entry(&session).unwrap();
        let mut drive = session.driver();
        drive.omit_observation_pauses();
        let mut report = ScenarioReport::new(ScenarioName::AbMeetingJoin);
        let result = pre_join(&mut session, &mut drive, &mut report, Duration::ZERO).await;
        assert_eq!(result.is_ok(), status == 200);
        tokio::time::timeout(Duration::from_secs(2), async {
            while session.held_requests().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(session.held_requests().len(), 2);
        assert!(
            session
                .held_requests()
                .iter()
                .all(|request| request.label.as_deref() == Some(ENTRY_HELD))
        );
        assert!(session.capture_dir().unwrap().join("prejoin.png").exists());
        assert!(session.capture_dir().unwrap().join("after-join.png").exists());
    }
}

#[tokio::test]
async fn meeting_join_rejects_missing_metadata_before_navigation() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = page_with(false, false, |_, _| None).await;
    let mut session = page.session(temp.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::AbMeetingJoin);
    let steps = Steps {
        mri_b: "",
        upn_b: "peer@example.invalid",
        nonce: "offline",
        after_join: Duration::ZERO,
        options: RunOptions {
            load_timeout: Duration::from_secs(2),
            display: crate::browser::Display::Headless,
            call: crate::scenario::ab_call::CallStep::Answer,
        },
    };
    assert!(
        steps
            .run(&mut session, "", &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("no MRI")
    );
    for body in [None, Some("{}"), Some("not JSON")] {
        assert!(
            steps
                .join(&mut session, body, &mut report)
                .await
                .unwrap_err()
                .to_string()
                .contains("no join link")
        );
    }
    assert!(page.commands().is_empty());
}

#[tokio::test]
async fn missing_prejoin_control_reports_the_cdp_failure_without_installing_start_holds() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = page_with(false, false, |command, _| {
        (command["method"] == "Runtime.evaluate").then(|| Err("synthetic prejoin context gone".into()))
    })
    .await;
    let mut session = page.session(temp.path(), false);
    let mut drive = session.driver();
    let mut report = ScenarioReport::new(ScenarioName::AbMeetingJoin);
    assert!(
        pre_join(&mut session, &mut drive, &mut report, Duration::ZERO)
            .await
            .is_err()
    );
    assert!(session.held_requests().is_empty());
}

use crate::scenario::meeting_page;

fn join_steps() -> Steps<'static> {
    Steps {
        mri_b: "8:orgid:00000000-0000-0000-0000-00000000000b",
        upn_b: "peer@example.invalid",
        nonce: "offline",
        after_join: Duration::ZERO,
        options: RunOptions {
            load_timeout: Duration::from_secs(2),
            display: crate::browser::Display::Headless,
            call: crate::scenario::ab_call::CallStep::Answer,
        },
    }
}

#[tokio::test]
async fn own_meeting_is_cancelled_after_successful_and_failed_join_attempts() {
    for status in [200, 503] {
        let temp = tempfile::tempdir().unwrap();
        let mut page = meeting_page::page(meeting_page::creation(), 200, status).await;
        let mut session = page.session(temp.path(), true);
        let mut report = ScenarioReport::new(ScenarioName::AbMeetingJoin);
        let result = join_steps()
            .run(
                &mut session,
                "8:orgid:00000000-0000-0000-0000-00000000000a",
                &mut report,
            )
            .await;
        assert_eq!(result.is_ok(), status == 200, "{result:?}");
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("event's cancellation:") && note.contains("Some(200)"))
        );
        assert!(page.commands().iter().any(|command| {
            command["params"]["url"]
                .as_str()
                .is_some_and(|url| url.contains("meeting_synthetic"))
        }));
        assert_eq!(
            session.capture_dir().unwrap().join("join-failure.png").exists(),
            status != 200
        );
    }
}

#[tokio::test]
async fn incomplete_creation_metadata_still_attempts_cleanup_when_a_chat_is_known() {
    for body in [
        json!({"schedulingServiceEvent":{"groupContext":{"threadId":meeting_page::CHAT}}}),
        json!({"schedulingServiceEvent":{"links":{"join":meeting_page::LINK}}}),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut page = meeting_page::page(body.clone(), 200, 200).await;
        let mut session = page.session(temp.path(), false);
        let mut report = ScenarioReport::new(ScenarioName::AbMeetingJoin);
        let result = join_steps()
            .run(
                &mut session,
                "8:orgid:00000000-0000-0000-0000-00000000000a",
                &mut report,
            )
            .await;
        assert!(result.is_err());
        let known_chat = body["schedulingServiceEvent"]["groupContext"]["threadId"].is_string();
        assert_eq!(
            report.notes.iter().any(|note| note.contains("event's cancellation:")),
            known_chat
        );
    }
}

fn send_prejoin_requests(events: &EventsOut, status: u16) {
    for (id, path, body, held) in [
        (
            "subscription",
            "https://api.synthetic.invalid/api/v2/epconv",
            "subscription",
            false,
        ),
        (
            "media",
            "https://api.synthetic.invalid/api/v2/epconv",
            "callInvitation",
            true,
        ),
        (
            "entry",
            "https://synthetic.conv.skype.com/conv/synthetic/notificationLinks",
            "entry",
            true,
        ),
    ] {
        let request = json!({"method":"POST","url":path,"postData":body});
        broadcast(
            events,
            "Network.requestWillBeSent",
            json!({"requestId":id,"request":request}),
        );
        if held {
            broadcast(
                events,
                "Fetch.requestPaused",
                json!({"requestId":format!("paused-{id}"),"networkId":id,"resourceType":"XHR","request":request}),
            );
        } else {
            broadcast(
                events,
                "Network.responseReceived",
                json!({"requestId":id,"response":{"status":status}}),
            );
        }
    }
}
