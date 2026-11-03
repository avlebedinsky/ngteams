//! Everyone picker and held-before-send decisions on real armed, synthetic CDP sessions.

use serde_json::json;

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::CallStep;
use crate::scenario::ab_chat_full::tests::{EventsOut, broadcast, page, page_with};
use crate::scenario::test_page::{Page, evaluated};

const CHAT: &str = "19:synthetic-ab@thread.v2";
const NONCE: &str = "synthetic-everyone";
const POST: &str =
    "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19:synthetic-ab@thread.v2/messages";

#[derive(Clone, Copy)]
enum Picker {
    Offered,
    Absent,
    NeverHighlighted,
}

struct EveryonePeer {
    picker: Picker,
    admissible: bool,
    text: String,
    selected: usize,
    id: usize,
}

impl EveryonePeer {
    fn evaluate(&self, expression: &str) -> Option<Result<Value, String>> {
        if expression.contains("everyoneLines") {
            return Some(Ok(evaluated(
                json!({"open":true,"count":2,"selected":self.selected,"everyone":1,"everyoneLines":[["Everyone"]],"lineCounts":[1,1]}),
            )));
        }
        if expression.contains("return i >= 0") && expression.contains(EVERYONE) {
            return Some(Ok(evaluated(json!(
                self.selected > 0 && !matches!(self.picker, Picker::NeverHighlighted)
            ))));
        }
        if expression.contains(".some((e) =>") && expression.contains(EVERYONE) {
            return Some(match self.picker {
                Picker::Absent => Err("synthetic picker unavailable".into()),
                _ => Ok(evaluated(json!(true))),
            });
        }
        None
    }

    fn post(&mut self, events: &EventsOut) {
        self.id += 1;
        let id = format!("everyone-{}", self.id);
        let mri = if self.admissible {
            CHAT
        } else {
            "8:orgid:synthetic-outsider"
        };
        let mentions =
            json!([{"@type":"http://schema.skype.com/Mention","itemid":"0","mri":mri,"mentionType":"everyone"}])
                .to_string();
        let body = json!({"messagetype":"RichText/Html","content":format!("<p><at id=\"0\">Everyone</at>{}</p>",self.text),"properties":{"mentions":mentions}}).to_string();
        let request = json!({"method":"POST","url":POST,"headers":{},"postData":body});
        broadcast(
            events,
            "Network.requestWillBeSent",
            json!({"requestId":id,"request":request}),
        );
        if self.text.trim_start().starts_with(HELD_MARKER) {
            broadcast(
                events,
                "Fetch.requestPaused",
                json!({"requestId":id,"networkId":id,"request":request,"resourceType":"XHR"}),
            );
        } else {
            broadcast(
                events,
                "Network.responseReceived",
                json!({"requestId":id,"response":{"status":201}}),
            );
        }
    }

    fn answer(&mut self, command: &Value, events: &EventsOut) -> Option<Result<Value, String>> {
        match command["method"].as_str().unwrap() {
            "Runtime.evaluate" => self.evaluate(command["params"]["expression"].as_str().unwrap()),
            "Input.insertText" => {
                self.text = command["params"]["text"].as_str().unwrap().into();
                None
            }
            "Input.dispatchKeyEvent" if command["params"]["type"] == "keyDown" => {
                if command["params"]["key"] == "ArrowDown" {
                    self.selected += 1;
                    return Some(Ok(json!({})));
                }
                if command["params"]["key"] != "Enter" {
                    return None;
                }
                if self.text == "@" {
                    return Some(Ok(json!({})));
                }
                if self.text.contains("everyone") {
                    self.post(events);
                    Some(Ok(json!({})))
                } else {
                    None
                }
            }
            "Fetch.failRequest"
                if command["params"]["requestId"]
                    .as_str()
                    .unwrap()
                    .starts_with("everyone-") =>
            {
                broadcast(
                    events,
                    "Network.loadingFailed",
                    json!({"requestId":command["params"]["requestId"],"errorText":"net::ERR_BLOCKED_BY_CLIENT"}),
                );
                Some(Ok(json!({})))
            }
            _ => None,
        }
    }
}

async fn everyone_page(picker: Picker, admissible: bool) -> Page {
    let mut peer = EveryonePeer {
        picker,
        admissible,
        text: String::new(),
        selected: 0,
        id: 0,
    };
    page_with(false, false, move |command, events| peer.answer(command, events)).await
}

fn options() -> RunOptions {
    RunOptions {
        display: Display::Headless,
        load_timeout: Duration::from_secs(1),
        call: CallStep::Answer,
    }
}

#[tokio::test]
async fn everyone_is_held_and_classified_before_a_real_mention_is_sent_or_the_run_stops() {
    for admissible in [true, false] {
        let mut a = everyone_page(Picker::Offered, admissible).await;
        let mut b = page(false, false).await;
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let mut a_session = a.session(a_dir.path(), true);
        let mut b_session = b.session(b_dir.path(), true);
        a_session.hold(
            crate::capture_hold::Hold::new("POST", super::super::ab_chat::MESSAGE_POST)
                .unwrap()
                .with_body_containing(&held_text(NONCE)),
        );
        let steps = Steps {
            group: CHAT,
            nonce: NONCE,
            options: options(),
        };
        let mut report = ScenarioReport::new(ScenarioName::AbcEveryone);
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            steps.run(&mut a_session, &mut b_session, &mut report),
        )
        .await
        .unwrap();
        assert_eq!(outcome.is_ok(), admissible, "{outcome:?}; {report:?}");
        assert_eq!(a_session.held_requests().len(), 1);
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("held: the post's mentions"))
        );
        let commands = a.commands();
        let sent = commands.iter().any(|command| {
            command["method"] == "Input.insertText"
                && command["params"]["text"]
                    .as_str()
                    .is_some_and(|text| text.trim_start() == sent_text(NONCE))
        });
        assert_eq!(
            sent, admissible,
            "an unclassified held mention must prevent the sent one"
        );
        if admissible {
            assert!(report.notes.contains(&"b: shows a's mention of everyone".into()));
            assert!(report.notes.iter().any(|note| note.contains("everyone-received.png")));
        } else {
            assert!(outcome.unwrap_err().to_string().contains("nothing more is sent"));
        }
    }
}

#[tokio::test]
async fn an_absent_everyone_offer_is_diagnosed_and_the_picker_and_compose_are_cleared() {
    let mut peer = everyone_page(Picker::Absent, true).await;
    let dir = tempfile::tempdir().unwrap();
    let session = peer.session(dir.path(), false);
    let steps = Steps {
        group: CHAT,
        nonce: NONCE,
        options: options(),
    };
    let mut report = ScenarioReport::new(ScenarioName::AbcEveryone);
    assert!(
        steps
            .mention_everyone(&mut peer.driver, &session, "never sent", true, &mut report)
            .await
            .is_err()
    );
    assert_eq!(report.notes.len(), 1);
    assert!(report.notes[0].contains("the picker showed"));
    let commands = peer.commands();
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["params"]["key"] == "Escape" && command["params"]["type"] == "keyDown")
            .count(),
        2
    );
    assert!(commands.iter().any(|command| command["params"]["key"] == "Backspace"));
}

#[tokio::test]
async fn a_highlight_that_never_reaches_everyone_exhausts_the_existing_keyboard_bound() {
    let mut peer = everyone_page(Picker::NeverHighlighted, true).await;
    let error = highlight_everyone(&mut peer.driver).await.unwrap_err();
    assert!(error.to_string().contains("never reached Everyone in 12 steps"));
    assert_eq!(
        peer.commands()
            .iter()
            .filter(|command| command["params"]["key"] == "ArrowDown" && command["params"]["type"] == "keyDown")
            .count(),
        MOST_STEPS
    );
}

#[tokio::test]
async fn the_opened_session_driver_collects_failures_and_closes_both_accounts_after_every_outcome() {
    for admissible in [true, false] {
        let mut a = everyone_page(Picker::Offered, admissible).await;
        let mut b = page(false, false).await;
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a_session = a.session(a_dir.path(), true);
        let b_session = b.session(b_dir.path(), true);
        let report = drive(CHAT, NONCE, options(), a_session, b_session).await;
        assert_eq!(report.accounts.len(), 2);
        assert_eq!(
            report
                .failures
                .iter()
                .filter(|failure| failure.contains("didn't confirm closing"))
                .count(),
            2
        );
        assert!(a.commands().iter().any(|command| command["method"] == "Browser.close"));
        assert!(b.commands().iter().any(|command| command["method"] == "Browser.close"));
        assert_eq!(report.accounts[0].held_requests.len(), 1);
        if admissible {
            assert_eq!(report.failures.len(), 2, "{report:?}");
        } else {
            assert!(
                report
                    .failures
                    .iter()
                    .any(|failure| failure.contains("nothing more is sent"))
            );
            assert!(report.notes.iter().any(|note| note.contains("page at the failure")));
        }
    }
}
