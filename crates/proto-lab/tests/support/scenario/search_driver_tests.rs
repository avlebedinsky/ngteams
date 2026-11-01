//! Complete read-only search orchestration through a real loopback CDP session.
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::scenario::test_page::{Page, evaluated};

type Wire = Arc<Mutex<Option<mpsc::UnboundedSender<Value>>>>;
struct Search {
    wire: Wire,
    next: usize,
    fail_tabs: bool,
}

impl Search {
    fn emit(&self, method: &str, params: Value) {
        let mut event = json!({"sessionId": "synthetic-page", "method": method});
        event["params"] = params;
        self.wire.lock().unwrap().as_ref().unwrap().send(event).unwrap();
    }

    fn request(&mut self, method: &str, url: &str) {
        self.next += 1;
        let id = format!("synthetic-search-{}", self.next);
        self.emit(
            "Network.requestWillBeSent",
            json!({"requestId": id, "request": {
                "method": method, "url": url, "headers": {},
            }}),
        );
        self.emit(
            "Network.responseReceived",
            json!({"requestId": id, "response": {"status": 200}}),
        );
    }

    fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        if expression.contains("join(' | ')") {
            return Ok(evaluated(json!(TABS.join(" | "))));
        }
        if expression.contains("const n =") {
            if self.fail_tabs {
                return Err("synthetic tab disappeared".into());
            }
            self.request("POST", "https://substrate.office.com/search/api/v2/query");
        }
        if expression.contains("const items =") {
            self.request("GET", "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/synthetic/messages?private=synthetic");
        }
        Ok(evaluated(json!(true)))
    }

    fn answer(&mut self, command: &Value) -> Result<Value, String> {
        match command["method"].as_str().unwrap() {
            "Page.navigate" => {
                assert_eq!(command["params"]["url"], crate::session::TEAMS_URL);
                self.emit(
                    "Page.frameNavigated",
                    json!({"frame": {"id": "search-frame", "url": crate::session::TEAMS_URL}}),
                );
                self.request(
                    "GET",
                    "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations",
                );
                self.emit("Page.frameStoppedLoading", json!({"frameId": "search-frame"}));
                Ok(json!({}))
            }
            "Runtime.evaluate" => self.evaluate(command["params"]["expression"].as_str().unwrap()),
            "Input.insertText" => {
                assert!(command["params"]["text"].as_str().is_some_and(|text| !text.is_empty()));
                self.request("POST", "https://substrate.office.com/search/api/v1/suggestions");
                Ok(json!({}))
            }
            "Input.dispatchKeyEvent" => {
                if command["params"]["key"] == "Enter" && command["params"]["type"] == "keyDown" {
                    self.request("POST", "https://substrate.office.com/search/api/v2/query");
                }
                Ok(json!({}))
            }
            "Page.captureScreenshot" if self.fail_tabs => Err("synthetic screenshot failure".into()),
            "Page.captureScreenshot" => Ok(json!({"data": "c3ludGhldGlj"})),
            "Emulation.setDeviceMetricsOverride" | "Browser.close" => Ok(json!({})),
            method => Err(format!("unexpected synthetic search command {method}")),
        }
    }
}

async fn page(fail_tabs: bool) -> Page {
    let wire: Wire = Arc::default();
    let mut state = Search {
        wire: Arc::clone(&wire),
        next: 0,
        fail_tabs,
    };
    let page = Page::new(move |command| state.answer(command)).await;
    wire.lock().unwrap().replace(page.sender());
    page
}

#[tokio::test]
async fn finishing_search_records_every_tab_and_message_read_then_preserves_the_close_failure() {
    for (fail_tabs, managed) in [(false, false), (false, true), (true, true)] {
        let mut peer = page(fail_tabs).await;
        let dir = tempfile::tempdir().unwrap();
        let (session, process) = if managed {
            let (session, pid) = peer.managed_session(dir.path(), true);
            (session, Some(pid))
        } else {
            (peer.session(dir.path(), true), None)
        };
        let capture = session.capture_dir().unwrap().to_path_buf();
        let report = finish(session, options(), super::tests::quick()).await;
        assert_eq!(report.accounts.len(), 1);
        assert_eq!(report.accounts[0].capture.as_ref(), Some(&capture));
        assert_eq!(peer.commands().last().unwrap()["method"], "Browser.close");
        assert_eq!(
            report
                .failures
                .iter()
                .any(|line| line.contains("didn't confirm closing")),
            !managed,
            "{:?}",
            report.failures
        );
        if let Some(pid) = process {
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
        assert!(report.notes.iter().any(|line| line.starts_with("a: search requests:")));
        if fail_tabs {
            assert!(
                report
                    .failures
                    .iter()
                    .any(|line| line.contains("synthetic tab disappeared"))
            );
            assert!(
                report
                    .notes
                    .iter()
                    .any(|line| line.contains("synthetic screenshot failure"))
            );
            assert!(!capture.join("search-closed.png").exists());
        } else {
            assert_eq!(report.failures.len(), usize::from(!managed), "{:?}", report.failures);
            assert!(
                report
                    .notes
                    .iter()
                    .any(|line| line.contains("opening a message found read: GET")),
                "{:?}",
                report.notes
            );
            assert!(
                report
                    .notes
                    .iter()
                    .filter(|line| line.contains("opening a message found read"))
                    .all(|line| !line.contains("private="))
            );
            for tab in TABS {
                assert!(
                    report.notes.iter().any(|line| line.starts_with(&format!("a: {tab}:"))),
                    "{:?}",
                    report.notes
                );
            }
            assert_eq!(std::fs::read(capture.join("search-closed.png")).unwrap(), b"synthetic");
            assert_eq!(
                std::fs::read(capture.join("search-message-opened.png")).unwrap(),
                b"synthetic"
            );
        }
        assert!(capture.join(crate::capture::EVENTS_FILE).is_file());
    }
}

fn options() -> RunOptions {
    RunOptions {
        display: crate::browser::Display::Headless,
        load_timeout: Duration::from_secs(1),
        call: crate::scenario::ab_call::CallStep::Answer,
    }
}
