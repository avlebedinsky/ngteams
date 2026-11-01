//! Offline search decisions against a synthetic CDP peer.

use serde_json::{Value, json};

use super::*;
use crate::cdp::{Event, SessionId};
use crate::scenario::test_page::{Page, evaluated};

#[tokio::test]
async fn the_keyboard_focus_path_does_not_click_a_box() {
    let mut page = Page::new(|_| Ok(evaluated(json!(true)))).await;
    let mut report = ScenarioReport::new(ScenarioName::Search);
    focus_box(&mut page.driver, &mut report, Duration::ZERO).await.unwrap();
    assert_eq!(report.notes, ["a: Ctrl+E focused the search box"]);
    let commands = page.commands();
    assert_eq!(commands.len(), 3);
    for (command, kind) in commands.iter().zip(["keyDown", "keyUp"]) {
        assert_eq!(command["method"], "Input.dispatchKeyEvent");
        assert_eq!(command["params"]["type"], kind);
        assert_eq!(command["params"]["modifiers"], 2);
        assert_eq!(command["params"]["key"], "e");
    }
    assert!(
        commands[2]["params"]["expression"]
            .as_str()
            .unwrap()
            .contains("document.activeElement")
    );
}

#[tokio::test]
async fn an_unfocused_box_is_clicked_and_a_missing_box_fails_the_step() {
    for (clicked, succeeds) in [(true, true), (false, false)] {
        let mut page = Page::new(move |command| {
            let expression = command["params"]["expression"].as_str().unwrap_or("");
            Ok(evaluated(json!(expression.contains("e.click()") && clicked)))
        })
        .await;
        let mut report = ScenarioReport::new(ScenarioName::Search);
        let outcome = focus_box(&mut page.driver, &mut report, Duration::ZERO).await;
        assert_eq!(outcome.is_ok(), succeeds);
        assert_eq!(page.commands().len(), 4);
        if succeeds {
            assert_eq!(report.notes, ["a: the search box was clicked"]);
        } else {
            assert!(outcome.unwrap_err().to_string().contains("no search box to focus"));
            assert!(report.notes.is_empty());
        }
    }
}

#[tokio::test]
async fn tab_selection_propagates_presence_and_quotes_the_requested_name() {
    for value in [json!(true), json!(false), Value::Null] {
        let wanted = value == json!(true);
        let mut page = Page::new(move |_| Ok(evaluated(value.clone()))).await;
        let name = "Synthetic \"quoted\" tab";
        assert_eq!(click_tab(&mut page.driver, name).await.unwrap(), wanted);
        let expression = page.commands()[0]["params"]["expression"].as_str().unwrap().to_owned();
        assert!(expression.contains(&js_string(name)));
        assert!(expression.contains("getClientRects().length > 0"));
        assert!(expression.contains("e.click()"));
    }
    let mut page = Page::new(|_| Err("synthetic evaluation failure".to_owned())).await;
    assert!(click_tab(&mut page.driver, "Messages").await.is_err());
}

#[tokio::test]
async fn tab_diagnostics_report_names_unknown_values_and_cdp_errors() {
    for (value, expected) in [
        (json!("Messages | People"), "a: the results' tabs: Messages | People"),
        (Value::Null, "a: the results' tabs: ?"),
    ] {
        let mut page = Page::new(move |_| Ok(evaluated(value.clone()))).await;
        assert_eq!(tabs_note(&mut page.driver).await, expected);
    }
    let mut page = Page::new(|_| Err("synthetic tabs failure".to_owned())).await;
    let note = tabs_note(&mut page.driver).await;
    assert!(note.starts_with("a: can't list the results' tabs:"));
    assert!(note.contains("synthetic tabs failure"));
}

fn event(method: &str, params: Value) -> Event {
    Event {
        session_id: Some(SessionId::new("synthetic-page")),
        method: method.to_owned(),
        params,
    }
}

fn request(page: &Page, id: &str, method: &str, url: &str) {
    page.events
        .send(event(
            "Network.requestWillBeSent",
            json!({
                "requestId": id, "request": {"method": method, "url": url},
            }),
        ))
        .unwrap();
}

#[tokio::test]
async fn search_answers_match_post_requests_after_the_steps_start() {
    let mut page = Page::new(|_| Ok(json!({}))).await;
    let url = "https://substrate.office.com/search/api/v2/query?synthetic=1";
    request(&page, "old", "POST", url);
    page.events
        .send(event(
            "Network.responseReceived",
            json!({"requestId": "old", "response": {"status": 200}}),
        ))
        .unwrap();
    let since = page.driver.seen_so_far();
    request(&page, "read", "GET", url);
    request(&page, "current", "POST", url);
    page.events
        .send(event(
            "Network.responseReceived",
            json!({"requestId": "current", "response": {"status": 202}}),
        ))
        .unwrap();
    let found = answered(&mut page.driver, "query", QUERY_URL, since, quick().answered)
        .await
        .unwrap();
    assert_eq!(found.request_id, "current");
    assert_eq!(found.status, Some(202));
    let diagnostic = note("query", &Ok(found));
    assert!(diagnostic.contains("answered Some(202)"));
    assert!(!diagnostic.contains("synthetic=1"));
    assert!(
        answered(&mut page.driver, "invalid", "[", 0, quick().answered)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_failed_search_is_an_answer_and_retains_its_failure_diagnostic() {
    let mut page = Page::new(|_| Ok(json!({}))).await;
    request(
        &page,
        "failed",
        "POST",
        "https://substrate.office.com/search/api/v1/suggestions",
    );
    page.events
        .send(event(
            "Network.loadingFailed",
            json!({"requestId": "failed", "errorText": "net::ERR_FAILED"}),
        ))
        .unwrap();
    let found = answered(&mut page.driver, "suggestions", SUGGESTIONS, 0, quick().answered)
        .await
        .unwrap();
    assert_eq!(found.failed.as_deref(), Some("net::ERR_FAILED"));
    assert!(note("suggestions", &Ok(found)).contains("net::ERR_FAILED"));
    let error = Err(ScenarioError::Setup("synthetic search failure".to_owned()));
    assert!(note("query", &error).contains("synthetic search failure"));
}

#[tokio::test]
async fn go_to_enters_the_query_and_exits_both_modes_with_escape() {
    let mut page = Page::new(|command| {
        assert!(matches!(
            command["method"].as_str(),
            Some("Input.dispatchKeyEvent" | "Input.insertText")
        ));
        Ok(json!({}))
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::Search);
    go_to(&mut session, &mut page.driver, &mut report, "synthetic chat", quick())
        .await
        .unwrap();
    let commands = page.commands();
    assert_eq!(commands.len(), 7);
    assert_eq!(commands[0]["params"]["key"], "g");
    assert_eq!(commands[0]["params"]["modifiers"], 3);
    assert_eq!(commands[2]["method"], "Input.insertText");
    assert_eq!(commands[2]["params"]["text"], "synthetic chat");
    for index in [3, 5] {
        assert_eq!(commands[index]["params"]["key"], "Escape");
    }
    assert!(report.failures.is_empty());
}

fn answered_request(events: &tokio::sync::mpsc::UnboundedSender<Event>, id: &str, url: &str) {
    events
        .send(event(
            "Network.requestWillBeSent",
            json!({
                "requestId": id, "request": {"method": "POST", "url": url},
            }),
        ))
        .unwrap();
    events
        .send(event(
            "Network.responseReceived",
            json!({
                "requestId": id, "response": {"status": 200},
            }),
        ))
        .unwrap();
}

#[tokio::test]
async fn suggesting_types_each_character_and_enter_waits_for_the_results_request() {
    use std::sync::{Arc, Mutex};
    let relay: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Event>>>> = Arc::new(Mutex::new(None));
    let callbacks = Arc::clone(&relay);
    let mut typed = 0;
    let mut page = Page::new(move |command| {
        let events = callbacks.lock().unwrap();
        let events = events.as_ref().unwrap();
        match command["method"].as_str().unwrap() {
            "Input.insertText" => {
                typed += 1;
                answered_request(
                    events,
                    &format!("suggestion-{typed}"),
                    "https://substrate.office.com/search/api/v1/suggestions",
                );
            }
            "Input.dispatchKeyEvent" => {
                assert_eq!(command["params"]["key"], "Enter");
                if command["params"]["type"] == "keyDown" {
                    answered_request(events, "query", "https://substrate.office.com/search/api/v2/query");
                }
            }
            method => panic!("unexpected synthetic command {method}"),
        }
        Ok(json!({}))
    })
    .await;
    relay.lock().unwrap().replace(page.events.clone());
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::Search);
    suggest_and_query(&mut session, &mut page.driver, &mut report, "ab", quick())
        .await
        .unwrap();
    assert_eq!(report.notes.len(), 2);
    assert!(report.notes[0].contains("suggestions:"));
    assert!(report.notes[1].contains("query:"));
    assert!(report.notes.iter().all(|note| note.contains("Some(200)")));
    let commands = page.commands();
    assert_eq!(commands.len(), 4);
    assert_eq!(commands[0]["params"]["text"], "a");
    assert_eq!(commands[1]["params"]["text"], "b");
    assert_eq!(commands[2]["params"]["key"], "Enter");
}

#[tokio::test]
async fn absent_tabs_and_missing_message_results_are_recorded_without_navigation() {
    let mut page = Page::new(|command| {
        assert_eq!(command["method"], "Runtime.evaluate");
        Ok(evaluated(json!(false)))
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = page.session(dir.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::Search);
    visit_tab(&mut session, &mut page.driver, &mut report, "Files", quick())
        .await
        .unwrap();
    open_first_message(&session, &mut page.driver, &mut report, Duration::ZERO).await;
    assert_eq!(report.notes, ["a: no Files tab shown", "a: no message found to open"]);
    assert_eq!(page.commands().len(), 2);
    let mut page = Page::new(|_| Err("synthetic result gone".to_owned())).await;
    open_first_message(&session, &mut page.driver, &mut report, Duration::ZERO).await;
    assert!(report.notes[2].contains("can't open a message found"));
}

pub(super) fn quick() -> SearchWaits {
    SearchWaits {
        settle: Duration::from_millis(50),
        shortcut: Duration::ZERO,
        character: Duration::ZERO,
        answered: Duration::from_secs(1),
    }
}

#[test]
fn production_search_waits_retain_the_original_intervals() {
    assert_eq!(
        SearchWaits::default(),
        SearchWaits {
            settle: Duration::from_secs(3),
            shortcut: Duration::from_millis(800),
            character: Duration::from_millis(400),
            answered: Duration::from_secs(30),
        }
    );
}
