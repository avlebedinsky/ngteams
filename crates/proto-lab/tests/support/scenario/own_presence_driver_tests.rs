//! Presence scenarios use synthetic CDP observations and the ordinary fail-closed fence.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::scenario::ab_call::CallStep;
use crate::scenario::test_page::{Page, evaluated};

const CHAT: &str = "19:00000000-0000-0000-0000-00000000000a_00000000-0000-0000-0000-00000000000b@unq.gbl.spaces";
const BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19:00000000-0000-0000-0000-00000000000a_00000000-0000-0000-0000-00000000000b@unq.gbl.spaces";
const ENDPOINT: &str = "https://teams.microsoft.com/api/ups/amer/v1/me/endpoints/";
const MANUAL: &str = "https://teams.microsoft.com/api/ups/amer/v1/me/forceavailability/";
type Wire = Arc<Mutex<Option<mpsc::UnboundedSender<Value>>>>;

struct Presence {
    wire: Wire,
    text: String,
    next: usize,
    account_open: bool,
    statuses_open: bool,
}

impl Presence {
    fn event(&self, method: &str, params: Value) {
        let mut event = json!({"sessionId": "synthetic-page", "method": method});
        event["params"] = params;
        self.wire.lock().unwrap().as_ref().unwrap().send(event).unwrap();
    }

    fn request(&mut self, method: &str, url: &str, body: &str, fenced: bool) {
        self.next += 1;
        let id = format!("synthetic-{}", self.next);
        let request = json!({"method": method, "url": url, "headers": {}, "postData": body});
        self.event(
            "Network.requestWillBeSent",
            json!({"requestId": id, "request": request}),
        );
        if fenced {
            self.event("Fetch.requestPaused", json!({"requestId": format!("paused-{id}"), "networkId": id, "resourceType": "XHR", "request": request}));
        } else {
            self.event(
                "Network.responseReceived",
                json!({"requestId": id, "response": {"status": 201}}),
            );
        }
    }

    fn navigate(&mut self) {
        self.event(
            "Page.frameNavigated",
            json!({"frame": {"id": "synthetic-frame", "url": crate::session::TEAMS_URL}}),
        );
        self.request("GET", &format!("{BASE}/messages"), "", false);
        self.request(
            "POST",
            "https://teams.microsoft.com/registrar/prod/V2/registrations",
            r#"{"appId":"TeamsCDLWebWorker"}"#,
            false,
        );
        self.request("PUT", ENDPOINT, "{}", true);
        self.event("Page.frameStoppedLoading", json!({"frameId": "synthetic-frame"}));
    }

    fn evaluate(&mut self, expression: &str) -> Value {
        let clicked = expression.contains("e.click()");
        if expression.contains("me-control-avatar-trigger") {
            self.account_open |= clicked;
            return evaluated(json!(true));
        }
        if expression.contains("set-presence-status-menu-item") {
            self.statuses_open |= clicked;
            return evaluated(json!(self.account_open));
        }
        let status = STATUSES
            .iter()
            .find(|(_, tid)| expression.contains(tid))
            .map(|(name, _)| *name);
        if status.is_some() || expression.contains("set-availability-reset-status") {
            let shown = self.statuses_open;
            if clicked {
                assert!(shown, "a status must be opened before it can be clicked");
                self.request(
                    "PUT",
                    MANUAL,
                    &json!({"availability": status.unwrap_or("reset")}).to_string(),
                    true,
                );
                self.statuses_open = false;
                self.account_open = false;
            }
            return evaluated(json!(shown));
        }
        evaluated(json!(true))
    }

    fn answer(&mut self, command: &Value) -> Result<Value, String> {
        match command["method"].as_str().unwrap() {
            "Page.navigate" => {
                self.navigate();
                Ok(json!({}))
            }
            "Emulation.setDeviceMetricsOverride" | "Browser.close" => Ok(json!({})),
            "Page.captureScreenshot" => Err("synthetic screenshot failure".into()),
            "Runtime.evaluate" => Ok(self.evaluate(command["params"]["expression"].as_str().unwrap())),
            "Input.insertText" => {
                self.text = command["params"]["text"].as_str().unwrap().to_owned();
                Ok(json!({}))
            }
            "Input.dispatchKeyEvent" => {
                if command["params"]["key"] == "Enter" && command["params"]["type"] == "keyDown" {
                    assert!(self.text.starts_with(super::super::ab_chat::PROBE_MARKER));
                    self.request(
                        "POST",
                        &format!("{BASE}/messages"),
                        &json!({"content": self.text}).to_string(),
                        true,
                    );
                }
                Ok(json!({}))
            }
            "Fetch.continueRequest" => {
                self.event("Network.responseReceived", json!({"requestId": command["params"]["requestId"].as_str().unwrap().trim_start_matches("paused-"), "response": {"status": 200}}));
                Ok(json!({}))
            }
            "Fetch.failRequest" => {
                assert_eq!(command["params"]["errorReason"], "BlockedByClient");
                self.event("Network.loadingFailed", json!({"requestId": command["params"]["requestId"].as_str().unwrap().trim_start_matches("paused-"), "errorText": "net::ERR_BLOCKED_BY_CLIENT"}));
                Ok(json!({}))
            }
            method => Err(format!("unexpected synthetic presence command {method}")),
        }
    }
}

async fn page() -> Page {
    let wire: Wire = Arc::default();
    let mut presence = Presence {
        wire: Arc::clone(&wire),
        text: String::new(),
        next: 0,
        account_open: false,
        statuses_open: false,
    };
    let page = Page::new_lab_chat(CHAT, move |command| presence.answer(command)).await;
    wire.lock().unwrap().replace(page.sender());
    page
}

fn steps(report_with: Report) -> Steps<'static> {
    Steps {
        mri_b: "8:orgid:00000000-0000-0000-0000-00000000000b",
        chat_id: Some(CHAT),
        nonce: "synthetic-presence",
        report_with,
        options: RunOptions {
            display: crate::browser::Display::Headless,
            load_timeout: Duration::from_secs(1),
            call: CallStep::Answer,
        },
        waits: PresenceWaits {
            observe: Duration::ZERO,
            status_hold: Duration::ZERO,
            report: Duration::from_secs(1),
            status_sent: Duration::from_secs(1),
            menu: Duration::from_secs(1),
            glance: Duration::from_millis(10),
        },
    }
}

#[test]
fn live_presence_waits_keep_the_documented_durations() {
    assert_eq!(
        PresenceWaits::default(),
        PresenceWaits {
            observe: Duration::from_mins(5),
            status_hold: Duration::from_secs(75),
            report: Duration::from_mins(3),
            status_sent: Duration::from_secs(20),
            menu: Duration::from_secs(15),
            glance: Duration::from_secs(2)
        }
    );
}

#[tokio::test]
async fn every_presence_mode_enforces_its_probe_and_only_its_selected_reports_are_held() {
    for mode in [
        Report::Activity,
        Report::Endpoint,
        Report::Both,
        Report::ManualHeld,
        Report::Manual,
    ] {
        let mut page = page().await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = page.session(dir.path(), false);
        let steps = steps(mode);
        deny_probes(&session, Some(&probe_text(steps.nonce))).unwrap();
        if let Some(hold) = mode.hold() {
            force_hold(&session, hold).unwrap();
        }
        let mut report = ScenarioReport::new(mode.scenario());
        steps.run(&mut session, &mut report).await.unwrap();
        assert!(report.failures.is_empty(), "{mode:?}: {:?}", report.failures);
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("probe post was denied by the fence"))
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.starts_with("a: presence endpoint PUTs:"))
        );
        let denials = session.denials();
        assert_eq!(
            denials
                .iter()
                .filter(|denial| denial.label.as_deref() == Some(THIS_RUN_PROBE))
                .count(),
            1
        );
        let held_statuses = denials
            .iter()
            .filter(|denial| denial.label.as_deref() == Some(HELD_MANUAL_STATUS))
            .count();
        assert_eq!(held_statuses, if mode == Report::ManualHeld { 5 } else { 0 });
        if mode.manual() {
            for status in ["Busy", "DoNotDisturb", "Away", "Available", "reset"] {
                assert!(
                    report
                        .notes
                        .iter()
                        .any(|note| note.starts_with(&format!("a: {status} chosen at unix")))
                );
            }
        } else {
            assert!(report.notes.iter().any(|note| note.starts_with("observing from unix")));
        }
        if mode == Report::Activity {
            assert!(
                denials
                    .iter()
                    .any(|denial| denial.label.as_deref() == Some(HELD_ENDPOINT_PUT))
            );
        }
    }
}

#[tokio::test]
async fn missing_endpoint_reports_fail_automatic_modes_and_are_diagnostic_in_manual_modes() {
    for mode in [Report::Activity, Report::Both, Report::Manual] {
        let mut page = Page::new(|_| Err("no page operation is required for this wait".to_owned())).await;
        let mut steps = steps(mode);
        steps.waits.report = Duration::ZERO;
        let mut report = ScenarioReport::new(mode.scenario());
        assert_eq!(
            steps.reported(&mut page.driver, &mut report).await.is_ok(),
            mode != Report::Both
        );
        if mode != Report::Both {
            assert!(report.notes[0].contains("no presence endpoint PUT"));
        }
    }
}

fn config(root: &std::path::Path, with_mris: bool) -> LabConfig {
    let text = ["a", "b", "c"]
        .map(|alias| {
            let mri = if with_mris {
                format!("mri = '8:orgid:00000000-0000-0000-0000-00000000000{alias}'\n")
            } else {
                String::new()
            };
            format!("[[accounts]]\nalias = '{alias}'\nupn = 'synthetic-{alias}@lab.example.com'\n{mri}")
        })
        .join("\n");
    LabConfig::parse(&text, root).unwrap()
}

#[tokio::test]
async fn finishing_presence_preserves_forced_denials_and_reports_the_real_close_failure() {
    let mut peer = page().await;
    let dir = tempfile::tempdir().unwrap();
    let session = peer.session(dir.path(), true);
    let capture = session.capture_dir().unwrap().to_path_buf();
    let quick = steps(Report::ManualHeld);
    let report = finish(
        session,
        &config(dir.path(), true),
        quick.options,
        quick.report_with,
        quick.waits,
    )
    .await;
    assert_eq!(report.accounts.len(), 1);
    assert_eq!(report.accounts[0].capture.as_ref(), Some(&capture));
    assert!(capture.join(crate::capture::EVENTS_FILE).is_file());
    assert!(report.notes.iter().any(|line| line.contains("enforcement: a's probe")));
    assert!(report.notes.iter().any(|line| line.contains("a: reset chosen")));
    assert_eq!(
        report.accounts[0]
            .denials
            .iter()
            .filter(|denial| denial.label.as_deref() == Some(HELD_MANUAL_STATUS))
            .count(),
        5
    );
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert!(
        report.failures[0].contains("didn't confirm closing"),
        "{:?}",
        report.failures
    );
    assert_eq!(peer.commands().last().unwrap()["method"], "Browser.close");
}

#[tokio::test]
async fn missing_presence_mris_still_attempt_screenshot_and_close_and_preserve_the_capture() {
    let mut peer = page().await;
    let dir = tempfile::tempdir().unwrap();
    let session = peer.session(dir.path(), true);
    let quick = steps(Report::Both);
    let report = finish(
        session,
        &config(dir.path(), false),
        quick.options,
        quick.report_with,
        quick.waits,
    )
    .await;
    assert_eq!(report.accounts.len(), 1);
    assert!(report.failures.iter().any(|line| line.contains("no MRI for a or b")));
    assert!(
        report
            .notes
            .iter()
            .any(|line| line.contains("synthetic screenshot failure"))
    );
    assert_eq!(
        peer.commands()
            .iter()
            .map(|command| command["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Page.captureScreenshot", "Browser.close"]
    );
}

#[tokio::test]
async fn confirmed_presence_shutdown_yields_a_clean_report_with_its_captured_decisions() {
    let mut peer = page().await;
    let root = tempfile::tempdir().unwrap();
    let (session, pid) = peer.managed_session(root.path(), true);
    let quick = steps(Report::Both);
    let report = finish(
        session,
        &config(root.path(), true),
        quick.options,
        quick.report_with,
        quick.waits,
    )
    .await;
    assert!(!report.failed(), "{:?}", report.failures);
    assert_eq!(report.accounts.len(), 1);
    assert!(
        report
            .notes
            .iter()
            .any(|line| line.contains("a presence endpoint PUT was answered"))
    );
    assert_eq!(report.accounts[0].denials.len(), 1);
    assert_eq!(report.accounts[0].denials[0].label.as_deref(), Some(THIS_RUN_PROBE));
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test]
async fn missing_status_controls_fail_with_menu_diagnostics_instead_of_clicking_an_unknown_control() {
    for diagnostics in [Some("synthetic-status"), None] {
        let mut peer = Page::new(move |command| {
            assert_eq!(command["method"], "Runtime.evaluate");
            let expression = command["params"]["expression"].as_str().unwrap();
            assert!(!expression.contains("e.click()"));
            Ok(evaluated(if expression.contains("new Set") {
                diagnostics.map_or(Value::Null, |text| json!(text))
            } else {
                json!(false)
            }))
        })
        .await;
        let mut plan = steps(Report::Manual);
        plan.waits.glance = Duration::from_secs(1);
        plan.waits.menu = Duration::from_secs(1);
        let mut report = ScenarioReport::new(ScenarioName::OwnPresenceManual);
        assert!(
            plan.choose(&mut peer.driver, &mut report, "Busy", Selector::Tid(STATUSES[0].1))
                .await
                .is_err()
        );
        assert_eq!(
            report.notes,
            [format!(
                "a: account menu controls on the page: {}",
                diagnostics.unwrap_or("?")
            )]
        );
        let commands = peer.commands();
        for tid in [
            STATUSES[0].1,
            "set-presence-status-menu-item",
            "me-control-avatar-trigger",
        ] {
            let evaluated = commands
                .iter()
                .filter(|command| {
                    command["params"]["expression"]
                        .as_str()
                        .is_some_and(|expression| expression.contains(tid) && expression.ends_with(" !== null"))
                })
                .count();
            // A repeated query proves the prior false reply completed before the deadline.
            assert!(
                evaluated >= 2,
                "missing-control predicate {tid} did not receive a false reply"
            );
        }
    }
    let peer = Page::new(|_| Err("synthetic menu vanished".into())).await;
    assert!(menu_tids(&peer.driver).await.contains("synthetic menu vanished"));
}

#[tokio::test]
async fn an_already_shown_status_can_be_chosen_without_reopening_its_menu_but_missing_network_evidence_is_noted() {
    let mut peer = Page::new(|command| {
        assert_eq!(command["method"], "Runtime.evaluate");
        let expression = command["params"]["expression"].as_str().unwrap();
        assert!(expression.contains(STATUSES[0].1));
        assert!(!expression.contains("me-control-avatar-trigger"));
        Ok(evaluated(json!(true)))
    })
    .await;
    let mut plan = steps(Report::ManualHeld);
    plan.waits.status_sent = Duration::ZERO;
    let mut report = ScenarioReport::new(ScenarioName::OwnPresenceManualHeld);
    plan.choose(&mut peer.driver, &mut report, "Busy", Selector::Tid(STATUSES[0].1))
        .await
        .unwrap();
    assert_eq!(peer.commands().len(), 2);
    assert!(report.notes[0].contains("no manual status request settled"));
}

#[tokio::test]
async fn unrelated_scenario_names_are_rejected_before_reading_any_config() {
    let root = tempfile::tempdir().unwrap();
    let error = run_named(
        &LabPaths::new(root.path().to_path_buf()),
        steps(Report::Both).options,
        ScenarioName::Search,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("is no own-presence scenario"));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
