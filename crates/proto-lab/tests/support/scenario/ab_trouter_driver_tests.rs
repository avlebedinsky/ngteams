//! The Trouter driver runs on synthetic armed pages, with real forced probe decisions.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::CallStep;
use crate::scenario::ab_chat_full::tests::page as chat_page;
use crate::scenario::test_page::{Page, evaluated};

const CHAT: &str = "19:synthetic-ab@thread.v2";
const BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19:synthetic-ab@thread.v2";
type Wire = Arc<Mutex<Option<mpsc::UnboundedSender<Value>>>>;

fn steps(chat_id: Option<&'static str>) -> Steps<'static> {
    Steps {
        mri_a: "8:orgid:synthetic-a",
        mri_b: "8:orgid:00000000-0000-0000-0000-00000000000b",
        chat_id,
        nonce: "synthetic-trouter",
        options: RunOptions {
            display: Display::Headless,
            load_timeout: Duration::from_secs(1),
            call: CallStep::Answer,
        },
        observe: Duration::ZERO,
        reaction_waits: ReactionWaits {
            quick: Duration::from_secs(1),
            action: Duration::from_secs(1),
        },
    }
}

#[test]
fn live_reaction_waits_keep_the_existing_intervals() {
    assert_eq!(
        ReactionWaits::default(),
        ReactionWaits {
            quick: Duration::from_secs(5),
            action: Duration::from_secs(20)
        }
    );
}

#[tokio::test]
async fn both_activity_modes_register_both_documents_before_live_chat_edit_and_reaction() {
    for activity in [Activity::Reported, Activity::HeldBack] {
        let mut a = chat_page(false, false).await;
        let mut b = chat_page(false, false).await;
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let mut sender = a.session(a_dir.path(), false);
        let mut receiver = b.session(b_dir.path(), false);
        let steps = steps(Some(CHAT));
        deny_probes(&sender, Some(&probe_text(steps.nonce))).unwrap();
        if activity == Activity::HeldBack {
            sender.force_deny_frames(hold_activity().unwrap());
            receiver.force_deny_frames(hold_activity().unwrap());
        }
        let mut report = ScenarioReport::new(scenario(activity));
        steps.run(&mut sender, &mut receiver, &mut report).await.unwrap();
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        for note in [
            "messaging endpoint registered",
            "message showed in b's open chat",
            "edit showed in b's open chat",
            "a reacted to its message with a like",
        ] {
            assert!(
                report.notes.iter().any(|line| line.contains(note)),
                "missing {note}: {:?}",
                report.notes
            );
        }
        assert_eq!(
            sender
                .denials()
                .iter()
                .filter(|denial| denial.label.as_deref() == Some(THIS_RUN_PROBE))
                .count(),
            1
        );
        let commands = b.commands();
        assert_eq!(
            commands
                .iter()
                .filter(|command| command["method"] == "Page.navigate")
                .count(),
            2
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.starts_with("b: endpoint registrations"))
        );
    }
}

#[tokio::test]
async fn no_recorded_chat_refuses_before_a_page_is_navigated() {
    let mut a = chat_page(false, false).await;
    let mut b = chat_page(false, false).await;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut sender = a.session(a_dir.path(), false);
    let mut receiver = b.session(b_dir.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::AbTrouter);
    assert!(
        steps(None)
            .run(&mut sender, &mut receiver, &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("has no MRI")
    );
    assert!(a.commands().is_empty());
    assert!(b.commands().is_empty());
}

#[tokio::test]
async fn edit_and_reaction_errors_are_both_retained_and_diagnosed() {
    let mut a = Page::new(|_| Err("synthetic message unavailable".to_owned())).await;
    let mut b = chat_page(false, false).await;
    let dir = tempfile::tempdir().unwrap();
    let session = a.session(dir.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::AbTrouter);
    steps(Some(CHAT))
        .edit_and_react(&session, &mut a.driver, &mut b.driver, &mut report)
        .await;
    assert_eq!(report.failures.len(), 2);
    assert!(report.failures[0].starts_with("the edit:"));
    assert!(report.failures[1].starts_with("the reaction:"));
    assert_eq!(report.notes.len(), 2);
    assert!(
        report
            .notes
            .iter()
            .all(|note| note.contains("can't list the message controls"))
    );
}

struct Reaction {
    wire: Wire,
    shown_after: u8,
    stage: u8,
    status: u16,
}

impl Reaction {
    fn emit(&self) {
        let wire = self.wire.lock().unwrap();
        let wire = wire.as_ref().unwrap();
        for (method, params) in [
            (
                "Network.requestWillBeSent",
                json!({"requestId": "reaction", "request": {"method": "PUT", "url": format!("{BASE}/messages/synthetic/properties?name=emotions")}}),
            ),
            (
                "Network.responseReceived",
                json!({"requestId": "reaction", "response": {"status": self.status}}),
            ),
        ] {
            wire.send(json!({"sessionId": "synthetic-page", "method": method, "params": params}))
                .unwrap();
        }
    }

    fn answer(&mut self, command: &Value) -> Result<Value, String> {
        match command["method"].as_str().unwrap() {
            "Runtime.evaluate" => {
                let expression = command["params"]["expression"].as_str().unwrap();
                if expression.contains("return [r.x + r.width / 2") {
                    return Ok(evaluated(json!([20, 30])));
                }
                let shown = self.stage >= self.shown_after;
                if shown && expression.contains("e.click()") {
                    self.emit();
                }
                Ok(evaluated(json!(shown)))
            }
            "Input.dispatchMouseEvent" => {
                let params = &command["params"];
                if params["type"] == "mouseReleased" {
                    self.stage = if params["button"] == "right" { 2 } else { 1 };
                }
                if params["type"] == "mouseMoved" {
                    assert!(params.get("clickCount").is_none());
                } else {
                    assert_eq!(params["clickCount"], 1);
                }
                Ok(json!({}))
            }
            method => Err(format!("unexpected synthetic reaction command {method}")),
        }
    }
}

async fn reaction_page(shown_after: u8, status: u16) -> Page {
    let wire: Wire = Arc::default();
    let mut reaction = Reaction {
        wire: Arc::clone(&wire),
        shown_after,
        stage: 0,
        status,
    };
    let page = Page::new(move |command| reaction.answer(command)).await;
    wire.lock().unwrap().replace(page.sender());
    page
}

#[tokio::test]
async fn reaction_falls_back_from_hover_to_left_click_then_context_menu_and_requires_a_successful_answer() {
    for (stage, status, expected) in [
        (0, 201, "its hover bar"),
        (1, 201, "a click on the message"),
        (2, 201, "its context menu"),
        (0, 500, "answered Some(500)"),
    ] {
        let mut page = reaction_page(stage, status).await;
        let dir = tempfile::tempdir().unwrap();
        let session = page.session(dir.path(), false);
        let mut drive = session.driver();
        let result = react(
            &session,
            &mut drive,
            "synthetic message",
            steps(Some(CHAT)).reaction_waits,
        )
        .await;
        if status < 300 {
            assert_eq!(result.unwrap(), expected);
        } else {
            assert!(result.unwrap_err().to_string().contains(expected));
        }
        let commands = page.commands();
        assert_eq!(
            commands
                .iter()
                .filter(|command| command["params"]["type"] == "mousePressed")
                .count(),
            usize::from(stage)
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| command["params"]["type"] == "mouseMoved")
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn recorded_trouter_runs_close_and_report_both_accounts_in_order_in_both_activity_modes() {
    for activity in [Activity::Reported, Activity::HeldBack] {
        let mut a = chat_page(false, false).await;
        let mut b = chat_page(false, false).await;
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let sender = a.session(a_dir.path(), true);
        let receiver = b.session(b_dir.path(), true);
        let report = finish(sender, receiver, activity, &steps(Some(CHAT))).await;
        assert_eq!(report.scenario, scenario(activity));
        assert_eq!(
            report.accounts.iter().map(|account| account.alias).collect::<Vec<_>>(),
            [Alias::A, Alias::B]
        );
        assert_eq!(
            report.failures.len(),
            2,
            "only the two real CloseUnconfirmed errors remain: {report:?}"
        );
        assert!(
            report
                .failures
                .iter()
                .all(|failure| failure.contains("didn't confirm closing"))
        );
        assert_eq!(report.accounts[0].denials.len(), 1);
        assert_eq!(report.accounts[0].denials[0].label.as_deref(), Some(THIS_RUN_PROBE));
        assert!(report.accounts[1].denials.is_empty());
        for (account, page) in report.accounts.iter().zip([&a, &b]) {
            assert!(
                account
                    .capture
                    .as_ref()
                    .unwrap()
                    .join(crate::capture::EVENTS_FILE)
                    .is_file()
            );
            assert!(
                page.commands()
                    .iter()
                    .any(|command| command["method"] == "Browser.close")
            );
            assert!(
                page.commands()
                    .iter()
                    .all(|command| command["method"] != "Page.captureScreenshot")
            );
        }
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("message showed in b's open chat"))
        );
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.starts_with("b: endpoint registrations"))
        );
    }
}

#[tokio::test]
async fn a_failed_step_and_failed_screenshot_still_close_and_report_both_accounts() {
    use crate::scenario::ab_chat_full::tests::page_with;
    let mut a = page_with(false, false, |command, _| {
        (command["method"] == "Page.captureScreenshot").then(|| Err("synthetic screenshot unavailable".to_owned()))
    })
    .await;
    let mut b = chat_page(false, false).await;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let sender = a.session(a_dir.path(), true);
    let receiver = b.session(b_dir.path(), true);
    let report = finish(sender, receiver, Activity::Reported, &steps(None)).await;
    assert_eq!(
        report.accounts.iter().map(|account| account.alias).collect::<Vec<_>>(),
        [Alias::A, Alias::B]
    );
    assert_eq!(
        report.failures.len(),
        3,
        "step error and both unconfirmed closes: {report:?}"
    );
    assert!(report.failures[0].contains("has no MRI"));
    assert!(report.failures[1].starts_with("a:") && report.failures[2].starts_with("b:"));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("synthetic screenshot unavailable"))
    );
    assert!(
        !report.accounts[0]
            .capture
            .as_ref()
            .unwrap()
            .join("failure.png")
            .exists()
    );
    assert!(
        report.accounts[1]
            .capture
            .as_ref()
            .unwrap()
            .join("failure.png")
            .is_file()
    );
    for page in [&a, &b] {
        let commands = page.commands();
        let shot = commands
            .iter()
            .position(|command| command["method"] == "Page.captureScreenshot")
            .unwrap();
        let close = commands
            .iter()
            .position(|command| command["method"] == "Browser.close")
            .unwrap();
        assert!(shot < close, "diagnostics precede closing each account");
    }
}

#[tokio::test]
async fn registration_timeout_counts_other_stacks_and_unreadable_bodies_but_keeps_document_boundaries() {
    let mut page = Page::new(|command| match command["method"].as_str().unwrap() {
        "Network.getRequestPostData" => Ok(json!({})),
        method => Err(format!("unexpected command {method}")),
    })
    .await;
    let url = "https://synthetic.registrar.teams.microsoft.com/registrar/prod/v2/registrations";
    for (id, body) in [
        ("earlier", Some(MESSAGING_REGISTRATION)),
        ("calling", Some("SkypeSpacesWeb")),
        ("unreadable", None),
        ("denied", Some(MESSAGING_REGISTRATION)),
    ] {
        let mut request = json!({"method":"POST", "url":url, "headers":{}});
        if let Some(body) = body {
            request["postData"] = json!(body);
        }
        page.events
            .send(crate::cdp::Event {
                session_id: Some(SessionId::new("synthetic-page")),
                method: "Network.requestWillBeSent".to_owned(),
                params: json!({"requestId":id,"request":request}),
            })
            .unwrap();
        page.events
            .send(crate::cdp::Event {
                session_id: Some(SessionId::new("synthetic-page")),
                method: "Network.responseReceived".to_owned(),
                params: json!({"requestId":id,"response":{"status": if id == "denied" { 403 } else { 202 }}}),
            })
            .unwrap();
    }
    page.driver.seen_so_far();
    let error = wait_registered_with(&mut page.driver, "new document registers", 1, Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("1 other stack's and 1 unreadable"),
        "{error}"
    );
    assert!(
        page.commands()
            .iter()
            .any(|command| command["params"]["requestId"] == "unreadable")
    );
    assert!(
        page.commands()
            .iter()
            .all(|command| command["params"]["requestId"] != "earlier")
    );
}

#[tokio::test]
async fn absent_messages_and_failed_mouse_commands_abort_reactions_without_clicking_unrelated_controls() {
    for absent in [true, false] {
        let point = if absent { Value::Null } else { json!([20, 30]) };
        let mut page = Page::new(move |command| {
            if command["method"] == "Runtime.evaluate" {
                return Ok(evaluated(point.clone()));
            }
            Err("synthetic mouse disabled".to_owned())
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let session = page.session(dir.path(), false);
        let error = react(
            &session,
            &mut page.driver,
            "synthetic message",
            steps(Some(CHAT)).reaction_waits,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains(if absent {
            "no element shows the message"
        } else {
            "can't use the mouse"
        }));
        assert!(
            page.commands()
                .iter()
                .all(|command| command["params"]["type"] != "mousePressed")
        );
    }
}

#[tokio::test]
async fn reaction_diagnostics_distinguish_empty_controls_from_an_unreadable_page() {
    for (value, expected) in [
        (
            json!("message-actions-more"),
            "a: message controls on the page: message-actions-more",
        ),
        (Value::Null, "a: message controls on the page: ?"),
    ] {
        let page = Page::new(move |_| Ok(evaluated(value.clone()))).await;
        assert_eq!(tids_note("a", &page.driver).await, expected);
    }
}
