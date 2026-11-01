//! Image driver fallback, denied media and cleanup against synthetic CDP pages.

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::CallStep;
use crate::scenario::ab_chat_full::tests::page as chat_page;

use crate::scenario::ab_chat_full::tests::attachments as peer;
use peer::{Kind, Unsent, Upload};

fn image() -> Image {
    Image {
        mri_a: "8:orgid:synthetic-a".to_owned(),
        mri_b: "8:orgid:00000000-0000-0000-0000-00000000000b".to_owned(),
        texts: Texts::new("synthetic-image"),
        probe: probe_text("synthetic-image"),
        options: RunOptions {
            display: Display::Headless,
            load_timeout: Duration::from_secs(1),
            call: CallStep::Answer,
        },
        waits: AttachmentWaits {
            attach: Duration::from_secs(1),
            upload: Duration::from_secs(1),
            settle: Duration::ZERO,
        },
    }
}

#[test]
fn attachment_defaults_keep_the_live_observation_intervals() {
    assert_eq!(
        AttachmentWaits::default(),
        AttachmentWaits {
            attach: PASTE_WAIT,
            upload: MEDIA_WAIT,
            settle: SETTLE
        }
    );
}

#[tokio::test]
async fn image_runs_use_paste_or_drop_and_observe_the_real_denied_media_before_recipient_readback() {
    for take in ["paste", "drop"] {
        let mut a = peer::page(Kind::Image, take, Upload::Denied, Unsent::Delete).await;
        let mut b = chat_page(false, false).await;
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let mut sender = a.session(a_dir.path(), true);
        let mut recipient = b.session(b_dir.path(), false);
        let scenario = image();
        deny_probes(&sender, Some(&scenario.probe)).unwrap();
        deny_media_writes(&sender).unwrap();
        let mut report = ScenarioReport::new(ScenarioName::AbImage);
        scenario
            .run(
                Some("19:synthetic-ab@thread.v2"),
                &mut sender,
                &mut recipient,
                &mut report,
            )
            .await
            .unwrap();
        assert!(report.failures.is_empty(), "{report:?}");
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains(&format!("took the image ({take})")))
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("unsent image: deleted from its actions")),
            "{report:?}"
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("message and not the probe"))
        );
        assert!(report.notes.iter().any(|note| note == "b: shows the caption: false"));
        assert!(sender.denials().iter().any(is_media_write));
        assert!(
            a.commands()
                .iter()
                .any(|command| command["method"] == "Fetch.failRequest")
        );
        assert!(recipient.denials().is_empty());
    }
}

#[tokio::test]
async fn paste_and_drop_rejection_do_not_fabricate_an_upload_and_missing_editors_fail_immediately() {
    for take in ["none", "gone", "error"] {
        let mut page = peer::page(Kind::Image, take, Upload::Missing, Unsent::Absent).await;
        let dir = tempfile::tempdir().unwrap();
        let session = page.session(dir.path(), false);
        let mut scenario = image();
        scenario.waits.upload = Duration::ZERO;
        let mut report = ScenarioReport::new(ScenarioName::AbImage);
        let error = scenario
            .send_image(&session, &mut page.driver, &mut report)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(match take {
            "none" => "tried no media write",
            "gone" => "compose box went away",
            _ => "synthetic attachment editor failure",
        }));
        assert!(session.denials().is_empty());
        if take == "none" {
            assert!(report.notes.iter().any(|note| note.contains("showed no image")));
        }
    }
}

#[tokio::test]
async fn reload_reports_absent_still_visible_and_unreadable_unsent_images_without_deleting_other_messages() {
    for unsent in [Unsent::Absent, Unsent::Stays, Unsent::Broken] {
        let mut page = peer::page(Kind::Image, "none", Upload::Missing, unsent).await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = page.session(dir.path(), false);
        let mut report = ScenarioReport::new(ScenarioName::AbImage);
        image().reload(&mut session, &mut report).await.unwrap();
        assert!(report.failures.is_empty());
        assert!(
            report.notes.iter().any(|note| note.contains(match unsent {
                Unsent::Absent => "none of this run's shown",
                Unsent::Stays => "still shown",
                _ => "couldn't delete it",
            })),
            "{report:?}"
        );
    }
}

#[tokio::test]
async fn admitted_image_runs_finalize_both_accounts_and_captures_after_success_or_partial_failure() {
    for (chat, taken, recorded) in [
        (Some("19:synthetic-ab@thread.v2"), "paste", true),
        (Some("19:synthetic-ab@thread.v2"), "gone", true),
        (None, "paste", false),
    ] {
        let mut a = peer::page(Kind::Image, taken, Upload::Denied, Unsent::Delete).await;
        let mut b = chat_page(false, false).await;
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (sender, sender_pid) = a.managed_session(a_dir.path(), recorded);
        let (recipient, recipient_pid) = b.managed_session(b_dir.path(), true);
        let report = finish(sender, recipient, chat, &image()).await;
        assert_eq!(report.accounts.len(), 2, "{report:?}");
        assert_eq!(report.accounts[0].alias, Alias::A);
        assert_eq!(report.accounts[1].alias, Alias::B);
        assert_eq!(report.accounts[0].capture.is_some(), recorded);
        assert!(report.accounts[1].capture.is_some());
        for pid in [sender_pid, recipient_pid] {
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
        if chat.is_some() && taken == "paste" {
            assert!(report.failures.is_empty(), "{report:?}");
            assert!(report.notes.iter().any(|note| note.ends_with("denied in all")));
            assert!(!report.accounts[0].denials.is_empty());
            assert!(
                !report.accounts[0]
                    .capture
                    .as_ref()
                    .unwrap()
                    .join("failure.png")
                    .exists()
            );
        } else {
            assert_eq!(report.failures.len(), 1, "{report:?}");
            assert!(report.failures[0].contains(if chat.is_none() {
                "has no MRI"
            } else {
                "compose box went away"
            }));
            for capture in report.accounts.iter().filter_map(|account| account.capture.as_ref()) {
                assert!(capture.join("failure.png").is_file());
            }
        }
        for page in [&a, &b] {
            let commands = page.commands();
            let close = commands
                .iter()
                .position(|command| command["method"] == "Browser.close")
                .unwrap();
            assert!(
                commands[close + 1..]
                    .iter()
                    .all(|command| command["method"] != "Page.captureScreenshot")
            );
        }
    }
}

#[tokio::test]
async fn an_image_inspection_error_is_not_treated_as_a_rejected_paste_or_retried_drop() {
    let mut page = peer::page_with(Kind::Image, "paste", Upload::Missing, Unsent::Absent, |command, _| {
        let expression = command["params"]["expression"].as_str()?;
        (expression == compose_has_image()).then(|| Err("synthetic image inspection failed".to_owned()))
    })
    .await;
    let error = put_image(&mut page.driver, "synthetic.png", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("image inspection failed"));
    assert_eq!(
        page.commands()
            .iter()
            .filter(|command| command["params"]["expression"]
                .as_str()
                .is_some_and(|e| e.contains("editor.dispatchEvent(new ClipboardEvent")))
            .count(),
        1
    );
}

#[tokio::test]
async fn an_observed_caption_post_is_reported_separately_from_the_denied_media_upload() {
    use crate::scenario::ab_chat_full::tests::broadcast;
    use serde_json::json;
    let scenario = image();
    let caption = scenario.texts.caption.clone();
    let mut page = peer::page_with(Kind::Image, "paste", Upload::Denied, Unsent::Absent, move |command, relay| {
        if command["method"] == "Input.dispatchKeyEvent" && command["params"]["key"] == "Enter" && command["params"]["type"] == "keyDown" {
            broadcast(relay, "Network.requestWillBeSent", json!({"requestId":"synthetic-caption","request":{"method":"POST",
                "url":"https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19:synthetic-ab@thread.v2/messages", "postData":caption,"headers":{}}}));
            broadcast(relay, "Network.responseReceived", json!({"requestId":"synthetic-caption","response":{"status":201}}));
            return Some(Ok(json!({})));
        }
        None
    }).await;
    let dir = tempfile::tempdir().unwrap();
    let session = page.session(dir.path(), true);
    deny_media_writes(&session).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbImage);
    scenario
        .send_image(&session, &mut page.driver, &mut report)
        .await
        .unwrap();
    assert!(session.denials().iter().any(is_media_write));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("message post with the caption went out: Some(201)"))
    );
}

#[tokio::test]
async fn upload_observation_ignores_earlier_denials_and_waits_for_a_new_real_forced_denial() {
    use serde_json::json;
    let mut page = peer::page(Kind::Image, "none", Upload::Missing, Unsent::Absent).await;
    let dir = tempfile::tempdir().unwrap();
    let session = page.session(dir.path(), false);
    deny_media_writes(&session).unwrap();
    for (id, delay) in [("earlier", Duration::ZERO), ("later", Duration::from_millis(20))] {
        let wire = page.sender();
        let send = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            wire.send(json!({"method":"Fetch.requestPaused","sessionId":"synthetic-page", "params":{
                "requestId":format!("paused-{id}"),"networkId":format!("synthetic-{id}"),"resourceType":"XHR",
                "request":{"method":"POST","url":"https://us-api.asm.skype.com/v1/objects/delayed/content/imgpsh","postData":"synthetic","headers":{}}
            }})).unwrap();
        });
        let before = usize::from(id == "later");
        let found = wait_media_write(&session, before, Duration::from_secs(1))
            .await
            .unwrap();
        send.await.unwrap();
        assert_eq!(found.label.as_deref(), Some(MEDIA_WRITE));
        assert_eq!(media_writes(&session).len(), before + 1);
    }
    assert!(wait_media_write(&session, 2, Duration::ZERO).await.is_none());
}
