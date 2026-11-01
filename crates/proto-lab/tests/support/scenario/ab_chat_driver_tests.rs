//! The two-page chat driver uses synthetic CDP pages and the real forced-deny fence.

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::CallStep;
use crate::scenario::ab_chat_full::tests::page;

fn steps(chat_id: Option<&'static str>) -> Steps<'static> {
    Steps {
        mri_a: "8:orgid:synthetic-a",
        mri_b: "8:orgid:00000000-0000-0000-0000-00000000000b",
        chat_id,
        nonce: "synthetic-basic-chat",
        options: RunOptions {
            display: Display::Headless,
            load_timeout: Duration::from_secs(1),
            call: CallStep::Answer,
        },
    }
}

#[tokio::test]
async fn the_chat_run_proves_its_deny_before_delivering_the_real_message_to_the_other_page() {
    let mut a = page(false, false).await;
    let mut b = page(false, false).await;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut sender = a.session(a_dir.path(), false);
    let mut recipient = b.session(b_dir.path(), false);
    let steps = steps(Some("19:synthetic-ab@thread.v2"));
    deny_probes(&sender, Some(&probe_text(steps.nonce))).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbChat);
    steps.run(&mut sender, &mut recipient, &mut report).await.unwrap();
    assert!(report.failures.is_empty());
    assert!(report.notes.iter().any(|note| note == "a's message reached b"));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("denied by the fence and failed in a's page"))
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("message and not the probe"))
    );
    assert_eq!(sender.denials().len(), 1);
    assert_eq!(sender.denials()[0].label.as_deref(), Some(THIS_RUN_PROBE));
    let commands = a.commands();
    let denied = commands
        .iter()
        .position(|command| command["method"] == "Fetch.failRequest")
        .unwrap();
    let real_message = commands
        .iter()
        .position(|command| {
            command["method"] == "Input.insertText" && command["params"]["text"] == message_text(steps.nonce)
        })
        .unwrap();
    assert!(denied < real_message);
    assert!(recipient.denials().is_empty());
}

#[tokio::test]
async fn a_missing_recorded_chat_refuses_before_any_compose_action() {
    let mut a = page(false, false).await;
    let mut b = page(false, false).await;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut sender = a.session(a_dir.path(), false);
    let mut recipient = b.session(b_dir.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::AbChat);
    let error = steps(None)
        .run(&mut sender, &mut recipient, &mut report)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("has no MRI for a or b"));
    for commands in [a.commands(), b.commands()] {
        assert!(commands.iter().all(|command| command["method"] != "Input.insertText"));
    }
}

#[tokio::test]
async fn the_opened_pair_is_always_closed_after_success_missing_identity_or_a_page_failure() {
    use crate::scenario::test_page::Page;
    for fail in ["none", "identity", "page"] {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let mut a = page(false, false).await;
        let mut b = if fail == "page" {
            Page::new(|_| Err("synthetic recipient disappeared".to_owned())).await
        } else {
            page(false, false).await
        };
        let sender = a.session(a_dir.path(), true);
        let recipient = b.session(b_dir.path(), true);
        let steps = steps((fail != "identity").then_some("19:synthetic-ab@thread.v2"));
        let report = run_sessions(&steps, sender, recipient).await;
        assert_eq!(report.accounts.len(), 2, "{report:?}");
        assert_eq!(
            report
                .failures
                .iter()
                .filter(|failure| failure.contains("didn't confirm closing"))
                .count(),
            2,
            "{report:?}"
        );
        if fail == "none" {
            assert!(report.notes.iter().any(|note| note == "a's message reached b"));
            assert_eq!(report.failures.len(), 2, "{report:?}");
        } else {
            assert!(
                report
                    .notes
                    .iter()
                    .any(|note| note.contains("screenshot") || note.contains("page at the failure"))
            );
            assert!(
                report
                    .failures
                    .iter()
                    .any(|failure| failure.contains(if fail == "identity" {
                        "has no MRI"
                    } else {
                        "synthetic recipient disappeared"
                    }))
            );
        }
        for commands in [a.commands(), b.commands()] {
            assert!(commands.iter().any(|command| command["method"] == "Browser.close"));
        }
    }
}

#[tokio::test]
async fn diagnostic_screenshots_are_private_and_never_overwrite_an_earlier_failure() {
    use crate::scenario::test_page::{Page, evaluated};
    use base64::Engine as _;
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"synthetic screenshot bytes";
    let mut peer = Page::new(|command| {
        Ok(if command["method"] == "Page.captureScreenshot" {
            serde_json::json!({"data":base64::engine::general_purpose::STANDARD.encode(b"synthetic screenshot bytes")})
        } else {
            evaluated(serde_json::json!(17))
        })
    })
    .await;
    let session = peer.session(dir.path(), true);
    let note = save_screenshot(Alias::A, &session).await.unwrap();
    assert!(note.contains("page at the failure"));
    let file = session.capture_dir().unwrap().join("failure.png");
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    let note = save_screenshot(Alias::A, &session).await.unwrap();
    assert!(note.contains("no screenshot of the failure"));
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let error = no_probe_deny(&session, &session.driver()).await;
    assert!(
        error
            .to_string()
            .contains("0 other forced deny(s); compose box holds 17 characters")
    );
}

#[tokio::test]
async fn a_successful_chat_report_requires_both_real_stand_in_processes_to_exit_and_be_reaped() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut a = page(false, false).await;
    let mut b = page(false, false).await;
    let (sender, a_pid) = a.managed_session(a_dir.path(), true);
    let (recipient, b_pid) = b.managed_session(b_dir.path(), true);
    let report = run_sessions(&steps(Some("19:synthetic-ab@thread.v2")), sender, recipient).await;
    assert!(report.failures.is_empty(), "{report:?}");
    assert_eq!(report.accounts.len(), 2);
    assert!(report.notes.iter().any(|note| note == "a's message reached b"));
    for pid in [a_pid, b_pid] {
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
}
