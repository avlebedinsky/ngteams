//! Synthetic page decisions exercise the actual post-enforcement scenario steps.

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::cdp::{Event, SessionId};
use crate::scenario::test_page::{Page, evaluated};

#[path = "attachment_page.rs"]
pub(in crate::scenario) mod attachments;

const CHAT: &str = "19:synthetic-ab@thread.v2";
const B: &str = "8:orgid:00000000-0000-0000-0000-00000000000b";
const BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19:synthetic-ab@thread.v2";

pub(in crate::scenario) type EventsOut =
    Arc<Mutex<Option<(mpsc::UnboundedSender<Event>, mpsc::UnboundedSender<Value>)>>>;

pub(in crate::scenario) fn broadcast(events: &EventsOut, method: &str, params: Value) {
    let relay = events.lock().unwrap();
    let (direct, wire) = relay.as_ref().unwrap();
    direct
        .send(Event {
            session_id: Some(SessionId::new("synthetic-page")),
            method: method.into(),
            params: params.clone(),
        })
        .unwrap();
    let mut event = json!({"sessionId":"synthetic-page","method":method});
    event["params"] = params;
    wire.send(event).unwrap();
}

fn emit(events: &EventsOut, id: usize, method: &str, path: &str, text: &str) {
    let body = json!({"content":text,"properties":{"mentions":json!([{"mri":B}]).to_string()}}).to_string();
    let request = json!({"method":method,"url":format!("{BASE}/{path}"),"headers":{},"postData":body});
    broadcast(
        events,
        "Network.requestWillBeSent",
        json!({"requestId":format!("synthetic-{id}"),"wallTime":1_700_000_000.0,"request":request}),
    );
    if text.starts_with(crate::scenario::ab_chat::PROBE_MARKER) {
        broadcast(
            events,
            "Fetch.requestPaused",
            json!({"requestId":format!("paused-{id}"),"networkId":format!("synthetic-{id}"),"resourceType":"XHR","request":request}),
        );
    } else {
        broadcast(
            events,
            "Network.responseReceived",
            json!({"requestId":format!("synthetic-{id}"),"response":{"status":201}}),
        );
    }
}

struct Chat {
    events: EventsOut,
    text: String,
    operation: &'static str,
    id: usize,
    likes: usize,
    fail_editor: bool,
    missing_name: bool,
}

impl Chat {
    fn emit(&mut self, method: &str, path: &str) {
        self.id += 1;
        emit(&self.events, self.id, method, path, &self.text);
    }

    fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        if expression.contains("r.left + r.width / 2") || expression.contains("r.x + r.width / 2") {
            return Ok(evaluated(json!([20, 30])));
        }
        if expression.contains("return e ?") {
            return Ok(evaluated(if self.missing_name {
                Value::Null
            } else {
                json!("Synthetic B")
            }));
        }
        if expression.starts_with("(document.body?.innerText") {
            return Ok(evaluated(json!(false)));
        }
        if expression == VISIBLE_TIDS {
            return Ok(evaluated(json!("chat-pane-message compose-box")));
        }
        if self.fail_editor && expression.contains("return editor.contains(document.activeElement)") {
            return Err("synthetic editor disappeared".into());
        }
        if expression == crate::scenario::chat_shapes::HOVER_REACTIONS {
            return Ok(evaluated(json!(["message-actions-like"])));
        }
        if expression == "window.__labTyping ?? null" {
            return Ok(evaluated(json!([])));
        }
        self.action(expression);
        Ok(evaluated(json!(true)))
    }

    fn action(&mut self, expression: &str) {
        if expression.contains("message-hover-action-edit") || expression.contains("message-actions-edit") {
            self.operation = "PUT";
        }
        if expression.contains("message-hover-action-quoted-reply") {
            self.operation = "POST";
        }
        if expression.contains("e.click()") && expression.contains("message-actions-like") {
            self.likes += 1;
            self.emit(
                if self.likes % 2 == 1 { "PUT" } else { "DELETE" },
                "messages/synthetic/properties?name=emotions",
            );
        }
        if expression.contains("e.click()") && expression.contains("message-hover-action-delete") {
            self.emit("DELETE", "messages/synthetic");
        }
    }

    fn answer(&mut self, command: &Value) -> Result<Value, String> {
        match command["method"].as_str().unwrap() {
            "Runtime.evaluate" => self.evaluate(command["params"]["expression"].as_str().unwrap()),
            "Input.insertText" => {
                self.text = command["params"]["text"].as_str().unwrap().to_owned();
                if self.text.is_empty() {
                    self.emit("PUT", "properties?name=consumptionhorizon");
                }
                Ok(json!({}))
            }
            "Input.dispatchKeyEvent" => {
                if command["params"]["key"] == "Escape" {
                    self.operation = "POST";
                }
                if command["params"]["key"] == "Enter"
                    && command["params"]["type"] == "keyDown"
                    && self.text != "Synthetic B"
                {
                    self.emit(
                        self.operation,
                        if self.operation == "PUT" {
                            "messages/synthetic"
                        } else {
                            "messages"
                        },
                    );
                    self.operation = "POST";
                }
                Ok(json!({}))
            }
            "Page.navigate" => {
                broadcast(
                    &self.events,
                    "Page.frameNavigated",
                    json!({"frame":{"id":"synthetic-frame","url":crate::session::TEAMS_URL}}),
                );
                self.emit("GET", "messages");
                self.id += 1;
                let id = format!("registration-{}", self.id);
                broadcast(
                    &self.events,
                    "Network.requestWillBeSent",
                    json!({"requestId":id,"request":{"method":"POST","url":"https://teams.microsoft.com/registrar/prod/V2/registrations","postData":r#"{"appId":"TeamsCDLWebWorker"}"#}}),
                );
                broadcast(
                    &self.events,
                    "Network.responseReceived",
                    json!({"requestId":id,"response":{"status":201}}),
                );
                broadcast(
                    &self.events,
                    "Page.frameStoppedLoading",
                    json!({"frameId":"synthetic-frame"}),
                );
                Ok(json!({"frameId":"synthetic-frame"}))
            }
            "Fetch.failRequest" => {
                let id = command["params"]["requestId"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("paused-")
                    .unwrap();
                broadcast(
                    &self.events,
                    "Network.loadingFailed",
                    json!({"requestId":format!("synthetic-{id}"),"errorText":"net::ERR_BLOCKED_BY_CLIENT","blockedReason":"inspector"}),
                );
                Ok(json!({}))
            }
            "Page.captureScreenshot" => Ok(json!({"data":"c3ludGhldGlj"})),
            _ => Ok(json!({})),
        }
    }
}

pub(in crate::scenario) async fn page(fail_editor: bool, missing_name: bool) -> Page {
    page_with(fail_editor, missing_name, |_, _| None).await
}

pub(in crate::scenario) async fn page_with(
    fail_editor: bool,
    missing_name: bool,
    mut hook: impl FnMut(&Value, &EventsOut) -> Option<Result<Value, String>> + Send + 'static,
) -> Page {
    let relay: EventsOut = Arc::new(Mutex::new(None));
    let mut chat = Chat {
        events: Arc::clone(&relay),
        text: String::new(),
        operation: "POST",
        id: 0,
        likes: 0,
        fail_editor,
        missing_name,
    };
    let hooked = Arc::clone(&relay);
    let page = Page::new_lab_chat("19:synthetic-ab@thread.v2", move |command| {
        hook(command, &hooked).unwrap_or_else(|| chat.answer(command))
    })
    .await;
    relay.lock().unwrap().replace((page.events.clone(), page.sender()));
    page
}

#[tokio::test]
async fn a_runs_all_post_enforcement_steps_and_reports_only_the_failed_edit_before_continuing() {
    for fail_editor in [false, true] {
        let mut page = page(fail_editor, false).await;
        let dir = tempfile::tempdir().unwrap();
        let session = page.session(dir.path(), true);
        let texts = Texts::new("synthetic-run");
        let writes = Writes::on(CHAT).unwrap();
        let mut report = ScenarioReport::new(ScenarioName::AbChatFull);
        let mut steps = Steps {
            writes: &writes,
            texts: &texts,
            probe: "synthetic-denied-probe",
            mri_b: B,
            report: &mut report,
        };
        steps.a(&session, &mut page.driver).await;
        assert_eq!(report.failures.len(), usize::from(fail_editor), "{report:?}");
        if fail_editor {
            assert!(report.failures[0].contains("a: edit:"));
        }
        for step in [
            "send",
            "react",
            "unreact",
            "mention",
            "reply",
            "react to the reply",
            "delete",
        ] {
            assert!(report.notes.contains(&format!("a: {step}: done")), "{report:?}");
        }
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("b offered by name, picked and mentioned"))
        );
        assert!(
            page.commands()
                .iter()
                .any(|command| command["params"]["text"] == format!(" {}", texts.mention))
        );
    }
}

#[tokio::test]
async fn b_reads_messages_without_the_denied_probe_and_reports_its_read_receipt() {
    let mut page = page(false, false).await;
    let texts = Texts::new("synthetic-read");
    let writes = Writes::on(CHAT).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbChatFull);
    let mut steps = Steps {
        writes: &writes,
        texts: &texts,
        probe: "synthetic-denied-probe",
        mri_b: B,
        report: &mut report,
    };
    steps
        .b_reads(&mut page.driver, "8:orgid:synthetic-a", &chat_requests(CHAT).unwrap())
        .await
        .unwrap();
    assert!(report.notes.iter().any(|note| note.contains("not the probe")));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("read receipt: sent and answered"))
    );
}

#[tokio::test]
async fn b_reports_navigation_and_reaction_failures_individually_and_does_not_stop_after_the_first() {
    let mut page = Page::new(|_| Err("synthetic page context gone".into())).await;
    let dir = tempfile::tempdir().unwrap();
    let session = page.session(dir.path(), true);
    let texts = Texts::new("synthetic-read-failure");
    let writes = Writes::on(CHAT).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbChatFull);
    let mut steps = Steps {
        writes: &writes,
        texts: &texts,
        probe: "synthetic-denied-probe",
        mri_b: B,
        report: &mut report,
    };
    steps
        .b(&session, "8:orgid:synthetic-a", &chat_requests(CHAT).unwrap())
        .await;
    assert_eq!(report.failures.len(), 2);
    assert!(report.failures[0].contains("b: read:"));
    assert!(report.failures[1].contains("b: react:"));
    assert_eq!(report.notes.len(), 4);
    for (step, notes) in ["read", "react"].into_iter().zip(report.notes.as_chunks::<2>().0) {
        assert!(notes[0].starts_with("b: no screenshot of the failure:"));
        assert!(notes[0].contains("Page.captureScreenshot failed: synthetic page context gone"));
        assert_eq!(notes[1], format!("b: {step}: visible data-tids: null"));
    }
    let commands = page.commands();
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["method"] == "Page.captureScreenshot")
            .count(),
        2
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["method"] == "Input.dispatchKeyEvent" && command["params"]["key"] == "Escape")
            .count(),
        4
    );
    assert!(!commands.iter().any(|command| command["method"] == "Input.insertText"));
}

#[tokio::test]
async fn a_missing_picker_name_cancels_the_picker_and_clears_compose_before_another_send() {
    let mut page = page(false, true).await;
    let mut notes = Vec::new();
    let error = mention_b(
        &mut page.driver,
        &Writes::on(CHAT).unwrap().post,
        B,
        "synthetic mention",
        &mut notes,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("picker shows no name for b"));
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("the picker showed"));
    let commands = page.commands();
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["method"] == "Input.dispatchKeyEvent"
                && command["params"]["key"] == "Escape"
                && command["params"]["type"] == "keyDown")
            .count(),
        2
    );
    assert!(commands.iter().any(|command| command["params"]["key"] == "Backspace"));
    send(
        &mut page.driver,
        &Writes::on(CHAT).unwrap().post,
        "synthetic recovery",
        "synthetic next post",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn the_complete_full_chat_flow_proves_the_forced_deny_before_its_independent_steps() {
    let mut a = page(false, false).await;
    let mut b = page(false, false).await;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut a_session = a.session(a_dir.path(), true);
    let mut b_session = b.session(b_dir.path(), true);
    let full = Full {
        mri_a: "8:orgid:synthetic-a".into(),
        mri_b: B.into(),
        nonce: "synthetic-complete".into(),
        texts: Texts::new("synthetic-complete"),
    };
    deny_probes(&a_session, Some(&probe_text(&full.nonce))).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbChatFull);
    let options = RunOptions {
        display: crate::browser::Display::Headless,
        load_timeout: Duration::from_secs(1),
        call: crate::scenario::ab_call::CallStep::Answer,
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        full.run(Some(CHAT), &mut a_session, &mut b_session, &mut report, options),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(report.failures.is_empty(), "{report:?}");
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("probe post was denied by the fence and failed"))
    );
    assert!(report.notes.iter().any(|note| note == "b: react: done"));
    assert_eq!(a_session.denials().len(), 1);
    assert_eq!(
        a_session.denials()[0].label.as_deref(),
        Some(super::super::ab_chat::THIS_RUN_PROBE)
    );
    assert!(
        a_session.close().await.is_err(),
        "an unowned synthetic browser cannot confirm its exit"
    );
    assert!(
        b_session.close().await.is_err(),
        "an unowned synthetic browser cannot confirm its exit"
    );
}

fn run_options() -> RunOptions {
    RunOptions {
        display: crate::browser::Display::Headless,
        load_timeout: Duration::from_secs(1),
        call: crate::scenario::ab_call::CallStep::Answer,
    }
}

fn unsigned_config(root: &std::path::Path) -> LabConfig {
    let accounts = ["a", "b", "c"]
        .map(|alias| format!("[[accounts]]\nalias = '{alias}'\nupn = 'synthetic-{alias}@example.test'\n"));
    LabConfig::parse(&accounts.join("\n"), root).unwrap()
}

#[tokio::test]
async fn setup_preserves_page_options_and_refuses_missing_identity_then_closes_both_real_processes() {
    let root = tempfile::tempdir().unwrap();
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut a = page(false, false).await;
    let mut b = page(false, false).await;
    let (first, a_pid) = a.managed_session(a_dir.path(), true);
    let (second, b_pid) = b.managed_session(b_dir.path(), true);
    let mut sessions = std::collections::VecDeque::from([first, second]);
    let mut opened = Vec::new();
    let report = run_with(&unsigned_config(root.path()), run_options(), async |alias, options| {
        opened.push(alias);
        assert!(options.live_members && options.capture_request_bodies);
        assert_eq!(options.capture.as_deref(), Some("ab-chat-full"));
        assert!(matches!(options.media, Media::System));
        assert_eq!(options.display, crate::browser::Display::Headless);
        assert!(options.user_agent.is_none());
        Ok(sessions.pop_front().unwrap())
    })
    .await
    .unwrap();
    assert_eq!(opened, [Alias::A, Alias::B]);
    assert_eq!(report.accounts.len(), 2);
    assert_eq!(report.failures.len(), 1, "{report:?}");
    assert!(report.failures[0].contains("no MRI for a or b"));
    for (peer, pid) in [(a, a_pid), (b, b_pid)] {
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert!(
            peer.commands()
                .iter()
                .any(|command| command["method"] == "Browser.close")
        );
        assert!(
            peer.commands()
                .iter()
                .all(|command| command["method"] != "Input.insertText")
        );
    }
}

#[tokio::test]
async fn failure_to_open_the_second_full_chat_page_closes_and_reaps_the_first() {
    let root = tempfile::tempdir().unwrap();
    let a_dir = tempfile::tempdir().unwrap();
    let mut a = page(false, false).await;
    let (first, pid) = a.managed_session(a_dir.path(), true);
    let mut first = Some(first);
    let mut opened = Vec::new();
    let error = run_with(&unsigned_config(root.path()), run_options(), async |alias, _| {
        opened.push(alias);
        first
            .take()
            .ok_or_else(|| SessionError::NoChrome("synthetic-second-full-page".into()))
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("synthetic-second-full-page"));
    assert_eq!(opened, [Alias::A, Alias::B]);
    assert_eq!(a.commands().len(), 1);
    assert_eq!(a.commands()[0]["method"], "Browser.close");
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test]
async fn full_chat_reports_keep_independent_steps_and_close_every_real_process_after_each_outcome() {
    for failure in ["none", "editor", "page"] {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let mut a = page(failure == "editor", false).await;
        let mut b = if failure == "page" {
            Page::new(|_| Err("synthetic recipient disappeared".to_owned())).await
        } else {
            page(false, false).await
        };
        let (sender, a_pid) = a.managed_session(a_dir.path(), true);
        let (recipient, b_pid) = b.managed_session(b_dir.path(), true);
        let full = Full {
            mri_a: "8:orgid:synthetic-a".into(),
            mri_b: B.into(),
            nonce: "synthetic-finalized-full".into(),
            texts: Texts::new("synthetic-finalized-full"),
        };
        let report = run_sessions(&full, Some(CHAT), run_options(), sender, recipient).await;
        assert_eq!(report.accounts.len(), 2);
        for pid in [a_pid, b_pid] {
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
        if failure == "none" {
            assert!(report.failures.is_empty(), "{report:?}");
        } else {
            assert!(
                report
                    .failures
                    .iter()
                    .any(|reason| reason.contains(if failure == "editor" {
                        "synthetic editor disappeared"
                    } else {
                        "synthetic recipient disappeared"
                    })),
                "{report:?}"
            );
        }
        if failure != "page" {
            assert!(report.notes.iter().any(|note| note == "b: react: done"));
            assert!(report.notes.iter().any(|note| note == "a: delete: done"));
        }
        for commands in [a.commands(), b.commands()] {
            assert!(commands.iter().any(|command| command["method"] == "Browser.close"));
        }
    }
}
