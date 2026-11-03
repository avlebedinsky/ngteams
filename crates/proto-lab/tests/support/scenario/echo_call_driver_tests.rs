//! Echo driver decisions on a synthetic armed page, with no browser, audio device, token or tenant.

use serde_json::json;

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::CallStep;
use crate::scenario::test_page::{Page, evaluated};

fn quick() -> Pauses {
    Pauses {
        steady: Duration::ZERO,
        warm: Duration::ZERO,
        echo_wait: Duration::ZERO,
        after_tone: Duration::ZERO,
        after_hang_up: Duration::ZERO,
    }
}

#[test]
fn observation_and_recording_defaults_stay_exactly_the_live_intervals() {
    assert_eq!(
        Pauses::default(),
        Pauses {
            steady: Duration::from_secs(8),
            warm: Duration::from_secs(15),
            echo_wait: Duration::from_secs(90),
            after_tone: Duration::from_secs(3),
            after_hang_up: Duration::from_secs(5)
        }
    );
}

async fn page(still_up: bool) -> Page {
    let mut hang_up_checks = 0;
    Page::new(move |command| match command["method"].as_str().unwrap() {
        "Page.navigate" => {
            assert_eq!(command["params"]["url"], crate::session::TEAMS_URL);
            Ok(json!({}))
        }
        "Emulation.setDeviceMetricsOverride" => {
            assert_eq!(command["params"]["width"], 1440);
            assert_eq!(command["params"]["height"], 900);
            Ok(json!({}))
        }
        "Runtime.evaluate" => {
            let expression = command["params"]["expression"].as_str().unwrap();
            let answer = if expression == "performance.timeOrigin" {
                json!(12345)
            } else if expression.contains("getAttribute('aria-checked')") && !expression.contains(" === ") {
                json!("true")
            } else if expression.contains("#hangup-button") && expression.ends_with(" !== null") {
                {
                    hang_up_checks += 1;
                    json!(still_up || hang_up_checks == 1)
                }
            } else {
                json!(true)
            };
            Ok(evaluated(answer))
        }
        method => Err(format!("unexpected echo driver command: {method}")),
    })
    .await
}

#[tokio::test]
async fn echo_prepares_the_stack_in_one_document_and_hangs_up_only_if_the_call_is_still_up() {
    for still_up in [true, false] {
        let mut page = page(still_up).await;
        let root = tempfile::tempdir().unwrap();
        let mut session = page.loaded_session(root.path(), false).await;
        let mut report = ScenarioReport::new(ScenarioName::EchoCall);
        let options = RunOptions {
            display: Display::Headless,
            load_timeout: Duration::from_secs(1),
            call: CallStep::Answer,
        };
        call(&mut session, options, &mut report, quick()).await.unwrap();
        assert!(report.notes.iter().any(|note| note.contains("calling stack; Settings")));
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("noise suppression is off"))
        );
        assert!(report.notes.iter().any(|note| note.contains(if still_up {
            "a: hung up"
        } else {
            "nothing to hang up"
        })));
        let commands = page.commands();
        let expressions = commands
            .iter()
            .filter_map(|command| command["params"]["expression"].as_str())
            .collect::<Vec<_>>();
        let placed = expressions
            .iter()
            .position(|expression| {
                expression.contains("start-echo-bot-call-component") && expression.contains("e.click()")
            })
            .unwrap();
        let warmed = expressions
            .iter()
            .position(|expression| expression.contains("performance.timeOrigin === 12345"))
            .unwrap();
        assert!(warmed < placed, "the same document is verified before a call is placed");
        let hangups = expressions
            .iter()
            .filter(|expression| expression.contains("#hangup-button") && expression.contains("e.click()"))
            .count();
        assert_eq!(hangups, usize::from(still_up));
    }
}

#[tokio::test]
async fn a_devices_click_that_no_longer_has_its_control_fails_instead_of_placing_a_call() {
    let mut page = Page::new(|command| {
        let expression = command["params"]["expression"].as_str().unwrap();
        Ok(evaluated(json!(!expression.contains("e.click()"))))
    })
    .await;
    let mut report = ScenarioReport::new(ScenarioName::EchoCall);
    let error = click_text(&mut page.driver, "synthetic devices", "Devices")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no control named"));
    assert!(report.failures.is_empty());
    assert_eq!(page.commands().len(), 2);
    // A missing screenshot is not substituted for a capture's signaling/media evidence.
    let root = tempfile::tempdir().unwrap();
    judge_capture(root.path(), &mut report);
    assert_eq!(report.failures.len(), 1);
    assert!(report.failures[0].contains("can't read the capture"));
}

#[test]
fn capture_judgment_requires_all_signaling_and_media_proofs() {
    let root = tempfile::tempdir().unwrap();
    let mut report = ScenarioReport::new(ScenarioName::EchoCall);
    fs::write(root.path().join(EVENTS_FILE), "{\"_synthetic\":true}\n").unwrap();
    judge_capture(root.path(), &mut report);
    assert_eq!(
        report.failures.len(),
        5,
        "start, add/leave, ack, duplex audio, and returned tone are independent"
    );
    let mut follow = CallFollowUps::default();
    for operation in ["add", "leave", "acknowledge"] {
        follow.allowed.insert(operation.to_owned(), 1);
    }
    let audio = AudioSummary {
        packets_sent: 2,
        packets_received: 3,
        received_energy: 0.1,
        longest_tone: TONE_READINGS,
        ..AudioSummary::default()
    };
    let mut good = ScenarioReport::new(ScenarioName::EchoCall);
    judge_follow_ups(&follow, &mut good);
    judge_audio(&audio, &mut good);
    assert!(good.failures.is_empty());
}

#[tokio::test]
async fn echo_finishes_the_capture_and_preserves_close_and_media_failures_after_a_step_fails() {
    let mut page = Page::new(|command| match command["method"].as_str().unwrap() {
        "Page.navigate" | "Emulation.setDeviceMetricsOverride" | "Browser.close" => Ok(json!({})),
        "Runtime.evaluate" => Err("synthetic echo page disappeared".to_owned()),
        "Page.captureScreenshot" => {
            use base64::Engine as _;
            Ok(json!({"data":base64::engine::general_purpose::STANDARD.encode(crate::scenario::ab_image::test_png(4))}))
        }
        method => Err(format!("unexpected echo cleanup command {method}")),
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let session = page.loaded_session(root.path(), true).await;
    let capture = session.capture_dir().unwrap().to_owned();
    let options = RunOptions {
        display: Display::Headless,
        load_timeout: Duration::from_secs(1),
        call: CallStep::Answer,
    };
    let report = drive(session, options, ScenarioReport::new(ScenarioName::EchoCall), quick()).await;
    assert_eq!(report.accounts.len(), 1);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.contains("synthetic echo page disappeared"))
    );
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.contains("close") || failure.contains("disappear"))
    );
    assert!(report.failures.iter().any(|failure| failure.contains("no call start")));
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.contains("acceptance wasn't acknowledged"))
    );
    assert!(report.notes.iter().any(|note| note.contains("recorded into")));
    assert!(capture.join("failure.png").is_file());
    assert!(
        page.commands()
            .iter()
            .any(|command| command["method"] == "Browser.close")
    );
}
