//! Form decisions against an offline CDP peer; no calendar or attendee is contacted.

use serde_json::{Value, json};

use super::*;
use crate::scenario::test_page::{Page, evaluated};

#[tokio::test]
async fn button_clicks_scope_the_search_and_refuse_missing_controls() {
    for present in [true, false] {
        let mut page = Page::new(move |_| Ok(evaluated(json!(present)))).await;
        let outcome = click_button(&mut page.driver, "save synthetic form", &["Save", "Send"]).await;
        assert_eq!(outcome.is_ok(), present);
        if let Err(error) = outcome {
            assert!(error.to_string().contains("no button named one of"));
        }
        let expression = page.commands()[0]["params"]["expression"].as_str().unwrap().to_owned();
        assert!(expression.contains("const root = document"));
        assert!(expression.contains("!e.disabled"));
        assert!(expression.contains("e.scrollIntoView"));
        assert!(expression.contains("e.click()"));
    }
    let mut page = Page::new(|_| Ok(evaluated(json!(true)))).await;
    click_in_dialog(&mut page.driver, "confirm synthetic form", &["Discard"])
        .await
        .unwrap();
    let expression = page.commands()[0]["params"]["expression"].as_str().unwrap().to_owned();
    assert!(expression.contains(".pop() || document"));
    assert!(expression.contains(&js_string(DIALOG)));
}

#[tokio::test]
async fn focus_and_wait_steps_report_missing_form_elements() {
    for present in [true, false] {
        let mut page = Page::new(move |_| Ok(evaluated(json!(present)))).await;
        let timeout = Duration::from_secs(1);
        assert_eq!(
            wait_for_button(&mut page.driver, "save shown", &["Save"], timeout)
                .await
                .is_ok(),
            present
        );
        assert_eq!(
            wait_for(&mut page.driver, "title shown", "#title", timeout)
                .await
                .is_ok(),
            present
        );
        let outcome = focus(&mut page.driver, "focus title", "#title").await;
        assert_eq!(outcome.is_ok(), present);
        if let Err(error) = outcome {
            assert!(error.to_string().contains("nothing matches #title"));
        }
        let commands = page.commands();
        let expressions: Vec<_> = commands
            .iter()
            .filter_map(|command| command["params"]["expression"].as_str())
            .collect();
        assert!(
            expressions
                .iter()
                .any(|expression| expression.contains("const names = [\"Save\"]"))
        );
        assert!(expressions.contains(&"document.querySelector(\"#title\") !== null"));
        assert_eq!(
            expressions
                .iter()
                .filter(|expression| expression.contains("e.focus()"))
                .count(),
            1
        );
        assert!(expressions.last().unwrap().contains("e.focus()"));
    }
}

#[tokio::test]
async fn cdp_form_errors_propagate_without_another_click() {
    let mut page = Page::new(|_| Err("synthetic missing execution context".to_owned())).await;
    assert!(click_button(&mut page.driver, "save", &["Save"]).await.is_err());
    assert!(focus(&mut page.driver, "title", "#title").await.is_err());
    assert_eq!(page.commands().len(), 2);
}

#[tokio::test]
async fn form_diagnostics_keep_unknown_and_failed_evaluations_visible() {
    for (value, expected) in [(json!("buttons: Save"), "a: buttons: Save"), (Value::Null, "a: ?")] {
        let mut page = Page::new(move |_| Ok(evaluated(value.clone()))).await;
        assert_eq!(controls_note(&mut page.driver).await, expected);
    }
    let mut page = Page::new(|_| Err("synthetic diagnostic failure".to_owned())).await;
    let note = controls_note(&mut page.driver).await;
    assert!(note.starts_with("a: can't list the page's controls:"));
    assert!(note.contains("synthetic diagnostic failure"));
}

#[test]
fn meeting_responses_need_success_and_expose_actionable_failures() {
    let seen = |status, failed| SeenRequest {
        session: None,
        request_id: "synthetic-request".to_owned(),
        method: "POST".to_owned(),
        url: "https://teams.microsoft.com/synthetic?private=query".to_owned(),
        status,
        failed,
        body: None,
        sent_ms: None,
    };
    for status in [200, 201, 204, 299] {
        let outcome = Ok(seen(Some(status), None));
        assert!(expect_success("save", &outcome).is_ok());
        let note = answer_note("save", &outcome);
        assert!(note.contains(&status.to_string()));
        assert!(!note.contains("private=query"));
    }
    for outcome in [
        Ok(seen(Some(400), None)),
        Ok(seen(None, Some("net::ERR_FAILED".to_owned()))),
        Err(ScenarioError::Setup("synthetic save failed".to_owned())),
    ] {
        assert!(expect_success("save", &outcome).is_err());
        assert!(answer_note("save", &outcome).contains("save"));
    }
}

type Relay = std::sync::Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>>;

fn form_steps() -> Steps<'static> {
    Steps {
        mri_b: "8:orgid:synthetic-b",
        upn_b: "synthetic-b@lab.example.com",
        chat_id: Some("19:synthetic@thread.v2"),
        nonce: "synthetic-meeting",
        pauses: FormPauses::default(),
        options: super::super::RunOptions {
            display: crate::browser::Display::Headless,
            load_timeout: Duration::from_secs(1),
            call: super::super::ab_call::CallStep::Answer,
        },
    }
}

fn form_write(relay: &Relay, method: &str, path: &str, status: u16, chat: &Value) {
    let sender = relay.lock().unwrap().as_ref().unwrap().clone();
    let body = json!({"schedulingServiceEvent": {"groupContext": {"threadId": chat}}}).to_string();
    let params = json!({"requestId": path, "request": {"method": method,
        "url": format!("https://teams.microsoft.com/api/mt/part/synthetic/v2.0/me/calendars/{path}"),
        "postData": body, "headers": {},
    }});
    for (method, params) in [
        ("Network.requestWillBeSent", params),
        (
            "Network.responseReceived",
            json!({"requestId": path, "response": {"status": status}}),
        ),
    ] {
        sender
            .send(json!({"sessionId": "synthetic-page", "method": method, "params": params}))
            .unwrap();
    }
}

async fn form_page(status: u16, chat: Value) -> Page {
    let relay: Relay = std::sync::Arc::new(std::sync::Mutex::new(None));
    let callbacks = std::sync::Arc::clone(&relay);
    let page = Page::new(move |command| {
        match command["method"].as_str().unwrap() {
            "Page.navigate" => {
                assert!(
                    command["params"]["url"]
                        .as_str()
                        .unwrap()
                        .starts_with("https://teams.microsoft.com/l/")
                );
                return Ok(json!({}));
            }
            "Input.insertText" | "Input.dispatchKeyEvent" => return Ok(json!({})),
            "Runtime.evaluate" => {}
            method => return Err(format!("unexpected synthetic form command {method}")),
        }
        let expression = command["params"]["expression"].as_str().unwrap();
        if expression == "-new Date().getTimezoneOffset()" {
            return Ok(evaluated(json!(180)));
        }
        if expression.contains("return 'buttons: '") {
            return Ok(evaluated(json!("buttons: Send | Save | Cancel meeting")));
        }
        if expression.contains("e.scrollIntoView") {
            if expression.contains("[\"Send\",\"Save\"]") {
                form_write(&callbacks, "POST", "default/events?synthetic=1", status, &chat);
            } else if expression.contains("Send update") {
                form_write(&callbacks, "PUT", "events/synthetic?synthetic=1", status, &chat);
            } else if expression.contains(".pop() || document") {
                form_write(&callbacks, "PUT", "events/synthetic/cancel", status, &chat);
            }
        }
        Ok(evaluated(json!(true)))
    })
    .await;
    relay.lock().unwrap().replace(page.sender());
    page
}

#[tokio::test]
async fn the_form_lifecycle_creates_updates_and_cancels_only_after_the_corresponding_click() {
    let mut page = form_page(201, json!("19:meeting_synthetic@thread.v2")).await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.loaded_session(dir.path(), false).await;
    let mut drive = session.driver();
    let steps = form_steps();
    let mut report = super::super::ScenarioReport::new(super::super::ScenarioName::AbMeeting);
    let chat = steps
        .create(&session, &mut drive, &mut report, "synthetic meeting")
        .await
        .unwrap();
    assert_eq!(chat, "19:meeting_synthetic@thread.v2");
    steps
        .change(
            &mut session,
            &mut drive,
            &mut report,
            &chat,
            "synthetic revised meeting",
        )
        .await
        .unwrap();
    steps.cancel(&session, &mut drive, &mut report).await.unwrap();
    assert!(report.failures.is_empty());
    for kind in ["creation", "update", "cancellation"] {
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains(&format!("the event's {kind}:")) && note.contains("Some(201)"))
        );
    }
    let commands = page.commands();
    let texts: Vec<_> = commands
        .iter()
        .filter(|command| command["method"] == "Input.insertText")
        .map(|command| command["params"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["synthetic-b@lab.example.com", "synthetic revised meeting"]);
    assert!(commands.iter().any(|command| {
        command["params"]["url"]
            .as_str()
            .is_some_and(|url| url.contains("/l/meeting/new?") && url.contains("synthetic+meeting"))
    }));
    assert!(commands.iter().any(|command| {
        command["params"]["expression"]
            .as_str()
            .is_some_and(|expression| expression.contains(".pop() || document"))
    }));
}

#[tokio::test]
async fn creation_refuses_an_http_failure_or_a_body_without_the_meeting_chat() {
    for (status, chat, expected) in [
        (500, json!("19:meeting_synthetic@thread.v2"), "answered Some(500)"),
        (201, Value::Null, "named no meeting chat"),
    ] {
        let mut page = form_page(status, chat).await;
        let dir = tempfile::tempdir().unwrap();
        let session = page.loaded_session(dir.path(), false).await;
        let mut drive = session.driver();
        let mut report = super::super::ScenarioReport::new(super::super::ScenarioName::AbMeeting);
        let error = form_steps()
            .create(&session, &mut drive, &mut report, "synthetic meeting")
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
        assert_eq!(
            report
                .notes
                .iter()
                .filter(|note| note.contains("the event's creation:"))
                .count(),
            1
        );
    }
}

#[test]
fn live_form_pauses_remain_two_seconds_for_each_picker_step_and_three_before_cancellation() {
    assert_eq!(
        FormPauses::default(),
        FormPauses {
            suggestion: Duration::from_secs(2),
            invited: Duration::from_secs(2),
            cancel: Duration::from_secs(3),
        }
    );
}

fn complete_steps() -> Steps<'static> {
    let mut steps = form_steps();
    steps.chat_id = Some("19:synthetic-ab@thread.v2");
    steps.pauses = FormPauses {
        suggestion: Duration::ZERO,
        invited: Duration::ZERO,
        cancel: Duration::ZERO,
    };
    steps
}

async fn complete_form_page(status: u16) -> Page {
    complete_form_page_statuses([status; 3]).await
}

async fn complete_form_page_statuses(statuses: [u16; 3]) -> Page {
    use crate::scenario::ab_chat_full::tests::{broadcast, page_with};
    page_with(false, false, move |command, relay| {
        if command["method"] == "Page.navigate"
            && command["params"]["url"].as_str().is_some_and(|url| url.contains("/l/meeting/new?"))
        {
            // The form navigation has no chat read carrying the editor's previously submitted probe.
            return Some(Ok(json!({})));
        }
        if command["method"] != "Runtime.evaluate" { return None; }
        let expression = command["params"]["expression"].as_str().unwrap();
        if expression == "-new Date().getTimezoneOffset()" { return Some(Ok(evaluated(json!(180)))); }
        if expression.contains("return 'buttons: '") { return Some(Ok(evaluated(json!("buttons: Save | Cancel meeting")))); }
        if expression.contains("e.scrollIntoView") {
            let write = if expression.contains("[\"Send\",\"Save\"]") { Some(("POST", "default/events?synthetic=1")) }
                else if expression.contains("Send update") { Some(("PUT", "events/synthetic?synthetic=1")) }
                else if expression.contains(".pop() || document") { Some(("PUT", "events/synthetic/cancel")) } else { None };
            if let Some((method, path)) = write {
                let body = json!({"schedulingServiceEvent":{"groupContext":{"threadId":"19:meeting_synthetic@thread.v2"}}}).to_string();
                broadcast(relay, "Network.requestWillBeSent", json!({"requestId":path,"request":{"method":method,
                    "url":format!("https://teams.microsoft.com/api/mt/part/synthetic/v2.0/me/calendars/{path}"),"postData":body,"headers":{}}}));
                broadcast(relay, "Network.responseReceived", json!({"requestId":path,"response":{"status": statuses[if method == "POST" { 0 } else if path.contains("cancel") { 2 } else { 1 }]}}));
            }
        }
        None
    }).await
}

#[tokio::test]
async fn an_admitted_meeting_session_always_records_its_capture_and_close_failure_after_success_or_form_error() {
    for status in [201, 500] {
        let mut page = complete_form_page(status).await;
        let dir = tempfile::tempdir().unwrap();
        let session = page.session(dir.path(), true);
        let report = finish(session, &complete_steps()).await;
        assert_eq!(report.accounts.len(), 1);
        assert_eq!(report.accounts[0].alias, Alias::A);
        assert_eq!(
            report
                .failures
                .iter()
                .filter(|failure| failure.contains("didn't confirm closing"))
                .count(),
            1
        );
        let capture = report.accounts[0].capture.as_ref().unwrap();
        let events = std::fs::read_to_string(capture.join(crate::capture::EVENTS_FILE)).unwrap();
        assert!(events.contains("/calendars/default/events"));
        assert!(report.accounts[0].held_requests.is_empty());
        assert!(report.notes.iter().any(|note| note.starts_with("a: calendar writes:")));
        if status == 201 {
            assert_eq!(
                report.failures.len(),
                1,
                "synthetic sessions retain CloseUnconfirmed: {report:?}"
            );
            assert!(capture.join("meeting-cancelled.png").is_file());
            assert!(!capture.join("failure.png").exists());
        } else {
            assert!(
                report
                    .failures
                    .iter()
                    .any(|failure| failure.contains("answered Some(500)"))
            );
            assert!(capture.join("failure.png").is_file());
            assert!(
                report
                    .notes
                    .iter()
                    .any(|note| note == "a: buttons: Save | Cancel meeting")
            );
        }
        assert!(
            page.commands()
                .iter()
                .any(|command| command["method"] == "Browser.close")
        );
    }
}

#[tokio::test]
async fn meeting_cleanup_judges_a_mapped_calendar_deny_even_when_identity_failure_stops_the_steps() {
    let mut page = complete_form_page(201).await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), true);
    session.hold(
        crate::capture_hold::Hold::new("POST", CALENDAR_WRITES)
            .unwrap()
            .labelled("synthetic calendar denial"),
    );
    page.send("Fetch.requestPaused", json!({"requestId":"paused-extra","networkId":"synthetic-extra","resourceType":"XHR",
        "request":{"method":"POST","url":"https://teams.microsoft.com/api/mt/part/synthetic/v2.0/me/calendars/default/events?synthetic=1","postData":"synthetic","headers":{}}}));
    let until = tokio::time::Instant::now() + Duration::from_secs(1);
    while session.held_requests().is_empty() {
        assert!(tokio::time::Instant::now() < until);
        session.pump(Duration::from_millis(10)).await;
    }
    let mut steps = complete_steps();
    steps.chat_id = None;
    let report = finish(session, &steps).await;
    assert_eq!(report.accounts.len(), 1);
    assert!(report.failures.iter().any(|failure| failure.contains("has no MRI")));
    assert_eq!(report.accounts[0].held_requests.len(), 1);
    assert_eq!(
        report.accounts[0].held_requests[0].label.as_deref(),
        Some("synthetic calendar denial")
    );
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.contains("didn't confirm closing"))
    );
    assert_eq!(
        report.accounts[0].held_requests[0].label.as_deref(),
        Some("synthetic calendar denial")
    );
}

#[tokio::test]
async fn incomplete_meeting_details_keep_diagnostics_and_screenshots_without_sending_an_update() {
    use crate::scenario::ab_chat_full::tests::page_with;
    for missing in ["details", "title"] {
        let mut page = page_with(false, false, move |command, _| {
            let expression = command["params"]["expression"].as_str()?;
            if expression.contains("return 'buttons: '") {
                return Some(Ok(evaluated(json!("buttons: synthetic partial calendar"))));
            }
            if missing == "details" && expression.contains("[\"Details\",\"Meeting details\"]")
                || missing == "title"
                    && expression == format!("document.querySelector({}) !== null", js_string(TITLE_INPUT))
            {
                return Some(Err("synthetic detached calendar control".to_owned()));
            }
            None
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = page.session(dir.path(), true);
        let mut drive = session.driver();
        let mut report = ScenarioReport::new(ScenarioName::AbMeeting);
        let error = open_details(
            &mut session,
            &mut drive,
            &mut report,
            "19:meeting_synthetic@thread.v2",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains(if missing == "details" {
            "shows no Details"
        } else {
            "show no title"
        }));
        assert!(report.notes.iter().any(|note| note.contains("partial calendar")));
        assert!(
            session
                .capture_dir()
                .unwrap()
                .join(if missing == "details" {
                    "meeting-chat.png"
                } else {
                    "meeting-details.png"
                })
                .is_file()
        );
        assert!(
            page.commands()
                .iter()
                .all(|command| command["params"]["text"].is_null())
        );
    }
}

#[tokio::test]
async fn cleanly_closed_meetings_still_fail_for_a_partial_form_and_keep_the_recorded_step_evidence() {
    for status in [201, 500] {
        let mut page = complete_form_page(status).await;
        let dir = tempfile::tempdir().unwrap();
        let (session, pid) = page.managed_session(dir.path(), true);
        let report = finish(session, &complete_steps()).await;
        assert_eq!(report.accounts.len(), 1);
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert_eq!(report.failures.len(), usize::from(status == 500), "{report:?}");
        let capture = report.accounts[0].capture.as_ref().unwrap();
        assert!(capture.join("meeting-sent.png").is_file());
        assert_eq!(capture.join("failure.png").is_file(), status == 500);
    }
}

#[tokio::test]
async fn an_update_or_cancellation_failure_preserves_prior_successes_and_stops_further_form_writes() {
    for (statuses, failed_step) in [([201, 503, 201], "update"), ([201, 201, 409], "cancellation")] {
        let mut page = complete_form_page_statuses(statuses).await;
        let dir = tempfile::tempdir().unwrap();
        let (session, _) = page.managed_session(dir.path(), true);
        let report = finish(session, &complete_steps()).await;
        assert_eq!(report.failures.len(), 1, "{report:?}");
        assert!(report.failures[0].contains(&format!("event's {failed_step}")));
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("event's creation:") && note.contains("Some(201)"))
        );
        let capture = report.accounts[0].capture.as_ref().unwrap();
        assert!(capture.join("meeting-sent.png").is_file());
        assert!(capture.join("meeting-updated.png").is_file());
        assert_eq!(
            capture.join("meeting-cancelled.png").is_file(),
            failed_step == "cancellation"
        );
        assert!(capture.join("failure.png").is_file());
    }
}
