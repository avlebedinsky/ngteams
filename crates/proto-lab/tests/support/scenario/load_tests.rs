//! Offline load/report cleanup keeps the real synthetic-session close failure.
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::scenario::test_page::Page;
use crate::session::SessionReport;

type Wire = Arc<Mutex<Option<mpsc::UnboundedSender<Value>>>>;

fn options() -> RunOptions {
    RunOptions {
        display: crate::browser::Display::Headless,
        load_timeout: Duration::from_millis(20),
        call: crate::scenario::ab_call::CallStep::Answer,
    }
}

async fn peer(loaded: bool, navigate_fails: bool) -> Page {
    let wire: Wire = Arc::default();
    let events = Arc::clone(&wire);
    let page = Page::new(move |command| {
        match command["method"].as_str().unwrap() {
            "Page.navigate" if navigate_fails => return Err("synthetic navigation failure".into()),
            "Page.navigate" if loaded => {
                assert_eq!(command["params"]["url"], TEAMS_URL);
                for (method, params) in [
                    ("Page.frameNavigated", json!({"frame": {"id": "load-frame", "url": TEAMS_URL}})),
                    ("Network.requestWillBeSent", json!({"requestId": "load", "request": {"method": "GET", "url": "https://teams.microsoft.com/api/chatsvc/synthetic", "headers": {}}})),
                    ("Network.responseReceived", json!({"requestId": "load", "response": {"status": 200}})),
                    ("Page.frameStoppedLoading", json!({"frameId": "load-frame"})),
                ] {
                    events.lock().unwrap().as_ref().unwrap().send(json!({"method": method, "params": params, "sessionId": "synthetic-page"})).unwrap();
                }
            }
            "Page.navigate" | "Browser.close" => {},
            method => return Err(format!("unexpected synthetic load command {method}")),
        }
        Ok(json!({}))
    }).await;
    wire.lock().unwrap().replace(page.sender());
    page
}

#[tokio::test]
async fn every_load_outcome_attempts_real_close_and_finishes_the_synthetic_capture() {
    assert_eq!(LINGER, Duration::from_secs(20));
    for (loaded, navigate_fails) in [(true, false), (false, false), (false, true)] {
        let mut peer = peer(loaded, navigate_fails).await;
        let dir = tempfile::tempdir().unwrap();
        let session = peer.session(dir.path(), true);
        let capture = session.capture_dir().unwrap().to_path_buf();
        let error = finish(session, options(), Duration::from_millis(20)).await.unwrap_err();
        assert!(error.to_string().contains("didn't confirm closing"), "{error}");
        assert_eq!(
            peer.commands()
                .iter()
                .map(|command| command["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["Page.navigate", "Browser.close"]
        );
        let recorded = std::fs::read_to_string(capture.join(crate::capture::EVENTS_FILE)).unwrap();
        assert_eq!(recorded.contains("Network.responseReceived"), loaded);
    }
}

#[test]
fn report_assembly_keeps_load_status_capture_and_fence_failures_independent() {
    for state in [
        SessionState::Loaded,
        SessionState::Loading,
        SessionState::Refused(403),
        SessionState::SignInShown,
        SessionState::Elsewhere("synthetic.example".into()),
    ] {
        for breached in [false, true] {
            let run = AccountRun {
                alias: ACCOUNT,
                browser: SessionReport {
                    failures: if breached {
                        vec!["synthetic capture connection lost".into()]
                    } else {
                        Vec::new()
                    },
                },
                held_requests: vec![],
                capture: breached.then(|| std::path::PathBuf::from("/synthetic/capture")),
            };
            let loaded = state == SessionState::Loaded;
            let report = report(&state, options(), Some(401), run);
            assert_eq!(report.accounts.len(), 1);
            assert_eq!(report.failures.is_empty(), loaded && !breached);
            assert_eq!(report.failures.iter().any(|line| line.contains("didn't load")), !loaded);
            assert_eq!(
                report
                    .notes
                    .iter()
                    .any(|line| line.contains("recorded into /synthetic/capture")),
                breached
            );

            if state == SessionState::Loading {
                assert!(report.failures.iter().any(|line| line.contains("authz answered 401")));
            }
        }
    }
}

#[tokio::test]
async fn linger_handles_a_zero_interval_and_a_disconnected_peer() {
    let mut page = peer(false, false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), false);
    linger(&mut session, Duration::ZERO).await;
    drop(page);
    tokio::time::timeout(Duration::from_secs(1), linger(&mut session, Duration::from_secs(30)))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_managed_stand_in_is_genuinely_reaped_on_both_confirmed_and_rejected_close() {
    for confirms in [true, false] {
        let mut peer = Page::new(move |command| {
            assert_eq!(command["method"], "Browser.close");
            if confirms {
                Ok(json!({}))
            } else {
                Err("synthetic close rejected".into())
            }
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let (session, pid) = peer.managed_session(dir.path(), true);
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert_eq!(session.close_recorded().await.is_ok(), confirms);
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
}

#[tokio::test]
async fn confirmed_load_cleanup_returns_the_actual_load_report_or_original_navigation_error() {
    for (loaded, navigate_fails) in [(true, false), (false, false), (false, true)] {
        let mut peer = peer(loaded, navigate_fails).await;
        let dir = tempfile::tempdir().unwrap();
        let (session, pid) = peer.managed_session(dir.path(), true);
        let outcome = finish(session, options(), Duration::ZERO).await;
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        if navigate_fails {
            assert!(
                outcome
                    .unwrap_err()
                    .to_string()
                    .contains("synthetic navigation failure")
            );
        } else {
            let report = outcome.unwrap();
            assert_eq!(!report.failed(), loaded, "{:?}", report.failures);
            assert_eq!(report.accounts.len(), 1);
            assert!(
                report.accounts[0]
                    .capture
                    .as_ref()
                    .unwrap()
                    .join(crate::capture::EVENTS_FILE)
                    .exists()
            );
        }
    }
}
