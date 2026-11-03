//! Channel dialogs against a synthetic CDP peer; every request stays on the local wire.

use serde_json::{Value, json};

use super::*;
use crate::scenario::ab_chat_full::tests::{EventsOut, broadcast, page_with};
use crate::scenario::test_page::{Page, evaluated};

#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    NoTeams,
    NoMore,
    NoChannel,
    NoRow,
    NoPeople,
    Rejected,
    NullWords,
}

struct Dialog {
    fault: Fault,
    held: bool,
    id: usize,
}

impl Dialog {
    fn request(&mut self, events: &EventsOut, path: &str, hold: bool) {
        self.id += 1;
        let id = format!("synthetic-{}", self.id);
        let request = json!({"method":"POST","url":format!("https://teams.cloud.microsoft/api/mt/{path}"),"postData":"{\"displayName\":\"synthetic\"}"});
        broadcast(
            events,
            "Network.requestWillBeSent",
            json!({"requestId":id,"request":request}),
        );
        if hold {
            broadcast(
                events,
                "Fetch.requestPaused",
                json!({"requestId":format!("paused-{}",self.id),"networkId":id,"resourceType":"XHR","request":request}),
            );
        } else {
            let status = if self.fault == Fault::Rejected { 403 } else { 201 };
            broadcast(
                events,
                "Network.responseReceived",
                json!({"requestId":id,"response":{"status":status}}),
            );
        }
    }

    fn answer(&mut self, command: &Value, events: &EventsOut) -> Option<Result<Value, String>> {
        if command["method"] == "Input.insertText" && command["params"]["text"] == "ca" {
            self.request(events, "members/search", false);
        }
        if command["method"] != "Runtime.evaluate" {
            return None;
        }
        let expression = command["params"]["expression"].as_str().unwrap();
        self.evaluate(expression, events).map(|value| Ok(evaluated(value)))
    }

    fn evaluate(&mut self, expression: &str, events: &EventsOut) -> Option<Value> {
        if expression.contains("const teams =") {
            return Some(if self.fault == Fault::NoTeams || expression.contains("teams[1]") {
                Value::Null
            } else {
                json!(1)
            });
        }
        if expression.contains("if (!more)") {
            return Some(json!(self.fault != Fault::NoMore));
        }
        if expression.contains(".join(' | ')") {
            return Some(if self.fault == Fault::NullWords {
                Value::Null
            } else if self.fault == Fault::NoChannel {
                json!("Leave team")
            } else {
                json!("Add channel | Standard | Private")
            });
        }
        self.action(expression, events)
    }

    fn action(&mut self, expression: &str, events: &EventsOut) -> Option<Value> {
        if expression.contains("create-and-edit-submit-button") {
            self.request(events, "channelsWarmup", false);
            self.request(events, "teams/synthetic/channels", self.held);
            return Some(json!(true));
        }
        if expression.contains("const add =") {
            self.request(events, "channels/private/users", false);
            return Some(json!(true));
        }
        if expression.contains("input.setAttribute('data-lab-people'") {
            return Some(json!(self.fault != Fault::NoPeople));
        }
        if expression.contains("new RegExp(\"Add channel\"") || expression.contains("new RegExp(\"^Add channel$\"") {
            return Some(json!(self.fault != Fault::NoChannel));
        }
        if expression.contains("new RegExp") && expression.contains("role=\\\"row") {
            return Some(json!(self.fault != Fault::NoRow));
        }
        if expression.starts_with("!!document.querySelector") {
            return Some(json!(false));
        }
        if expression.contains("innerText.replace") {
            return Some(if self.fault == Fault::NullWords {
                Value::Null
            } else {
                json!("Synthetic channel dialog")
            });
        }
        None
    }
}

async fn dialog(held: bool, fault: Fault) -> Page {
    let mut dialog = Dialog { fault, held, id: 700 };
    page_with(false, false, move |command, events| dialog.answer(command, events)).await
}

fn options() -> RunOptions {
    RunOptions {
        load_timeout: Duration::from_secs(2),
        display: crate::browser::Display::Headless,
        call: crate::scenario::ab_call::CallStep::Answer,
    }
}

#[tokio::test]
async fn capture_exercises_every_dialog_shape_and_keeps_channel_writes_held() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = dialog(true, Fault::None).await;
    let mut session = page.session(temp.path(), true);
    for rule in channel_rules().unwrap() {
        session.hold(rule);
    }
    let mut drive = session.driver();
    drive.pause(Duration::ZERO).await;
    drive.omit_observation_pauses();
    let mut report = ScenarioReport::new(ScenarioName::ChannelCreate);
    steps(&mut session, &mut drive, &mut report, options()).await.unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let held = session.held_requests();
    assert_eq!(held.len(), 3);
    assert!(
        held.iter()
            .all(|request| request.label.as_deref() == Some(CHANNEL_WRITE))
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("people search:") && note.contains("201"))
    );
    let commands = page.commands();
    for at in 0..3 {
        assert!(
            commands
                .iter()
                .any(|command| command["params"]["text"] == format!("lab dialog shape {at}"))
        );
    }
    assert!(commands.iter().any(|command| command["method"] == "Fetch.failRequest"));
    assert!(session.capture_dir().unwrap().join("channel-types.png").exists());
}

#[tokio::test]
async fn explicit_writes_wait_for_creation_and_add_only_the_private_member() {
    for fault in [Fault::None, Fault::Rejected] {
        let temp = tempfile::tempdir().unwrap();
        let mut page = dialog(false, fault).await;
        let mut session = page.session(temp.path(), true);
        let mut drive = session.driver();
        drive.omit_observation_pauses();
        let mut report = ScenarioReport::new(ScenarioName::ChannelCreate);
        let settings = Writing {
            team: "Synthetic team".into(),
            member: "peer@example.invalid".into(),
        };
        write_steps(&mut session, &mut drive, &mut report, options(), &settings)
            .await
            .unwrap();
        assert!(session.held_requests().is_empty());
        assert_eq!(report.failures.len(), if fault == Fault::Rejected { 4 } else { 0 });
        let commands = page.commands();
        assert_eq!(
            commands
                .iter()
                .filter(|command| command["params"]["text"] == "peer")
                .count(),
            1
        );
        for (at, (kind, layout)) in ATTEMPTS.into_iter().enumerate() {
            assert!(
                commands
                    .iter()
                    .any(|command| command["params"]["text"] == format!("lab {kind} {layout} {at}"))
            );
        }
        assert!(
            report
                .notes
                .iter()
                .filter(|note| note.contains("POST") && note.contains("channels"))
                .all(|note| !note.contains("Warmup"))
        );
    }
}

#[tokio::test]
async fn missing_teams_menus_and_controls_return_actionable_errors() {
    for fault in [Fault::NoTeams, Fault::NoMore, Fault::NoChannel, Fault::NullWords] {
        let temp = tempfile::tempdir().unwrap();
        let mut page = dialog(false, fault).await;
        let session = page.session(temp.path(), false);
        let mut drive = session.driver();
        drive.omit_observation_pauses();
        let mut report = ScenarioReport::new(ScenarioName::ChannelCreate);
        assert!(find_team(&session, &mut drive, &mut report).await.is_err());
        if fault == Fault::NoChannel {
            assert!(open_dialog(&session, &mut drive, &mut report, 0).await.is_err());
            assert!(
                open_managed_dialog(&session, &mut drive, &mut report, "Synthetic")
                    .await
                    .is_err()
            );
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let mut page = dialog(false, Fault::NoRow).await;
    let session = page.session(temp.path(), false);
    let mut drive = session.driver();
    drive.omit_observation_pauses();
    let mut report = ScenarioReport::new(ScenarioName::ChannelCreate);
    assert!(
        open_managed_dialog(&session, &mut drive, &mut report, "Missing")
            .await
            .unwrap_err()
            .to_string()
            .contains("no team named")
    );
}

#[tokio::test]
async fn absent_people_box_is_a_report_failure_and_unknown_dialog_text_is_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = dialog(false, Fault::NoPeople).await;
    let session = page.session(temp.path(), false);
    let mut drive = session.driver();
    drive.omit_observation_pauses();
    let mut report = ScenarioReport::new(ScenarioName::ChannelCreate);
    add_member(&session, &mut drive, &mut report, 2, "peer@example.invalid")
        .await
        .unwrap();
    assert_eq!(report.failures, ["attempt 2: no people box"]);
    let mut page = dialog(false, Fault::NullWords).await;
    let session = page.session(temp.path(), false);
    let mut drive = session.driver();
    drive.omit_observation_pauses();
    show_dropdown(&session, &mut drive, &mut report).await.unwrap();
    leave_error(&mut drive, &mut report, 0).await.unwrap();
    assert!(report.notes.iter().any(|note| note.ends_with("after: ?")));
}

#[tokio::test]
async fn failed_page_commands_propagate_before_a_write_is_attempted() {
    let mut page = Page::new(|_| Err("synthetic context lost".into())).await;
    let mut report = ScenarioReport::new(ScenarioName::ChannelCreate);
    assert!(
        fill_form(&mut page.driver, &mut report, 0, "Standard", "Posts")
            .await
            .is_err()
    );
    assert!(press_create(&mut page.driver, &mut report, 0).await.is_err());
    assert!(created(&mut page.driver, &mut report, 0, CHANNEL_URL).await.is_err());
}

#[tokio::test]
async fn admitted_channel_sessions_preserve_captures_and_finalize_after_success_or_partial_failure() {
    for fault in [Fault::None, Fault::NoMore] {
        let temp = tempfile::tempdir().unwrap();
        let mut page = dialog(true, fault).await;
        let (session, _) = page.managed_session(temp.path(), true);
        let mut drive = session.driver();
        drive.omit_observation_pauses();
        let report = finish(session, drive, None, options()).await.unwrap();
        assert_eq!(report.accounts.len(), 1);
        assert_eq!(
            report.failures.is_empty(),
            fault == Fault::None,
            "{:?}",
            report.failures
        );
        assert!(report.accounts[0].capture.as_ref().unwrap().exists());
        assert_eq!(
            report.accounts[0].held_requests.len(),
            if fault == Fault::None { 3 } else { 0 }
        );
        assert!(
            page.commands()
                .iter()
                .any(|command| command["method"] == "Browser.close")
        );
    }
    let temp = tempfile::tempdir().unwrap();
    let mut page = dialog(false, Fault::None).await;
    let (session, _) = page.managed_session(temp.path(), false);
    let mut drive = session.driver();
    drive.omit_observation_pauses();
    let writing = Writing {
        team: "Synthetic".into(),
        member: "peer@example.invalid".into(),
    };
    let report = finish(session, drive, Some(&writing), options()).await.unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(report.accounts[0].held_requests.is_empty());
}

#[test]
fn writing_settings_are_read_only_from_the_isolated_child_environment() {
    for case in [
        "disabled",
        "missing-team",
        "blank-team",
        "missing-config",
        "bad-config",
        "valid",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("scenario::channel_create::driver_tests::writing_settings_child")
            .env_clear()
            .env("HOME", temp.path())
            .env("CHANNEL_SETTINGS_CASE", case);
        if case != "disabled" {
            command.env("PROTO_LAB_CHANNEL_WRITES", "1");
        }
        if case == "blank-team" {
            command.env("PROTO_LAB_TEAM", "   ");
        }
        if matches!(case, "missing-config" | "bad-config" | "valid") {
            command.env("PROTO_LAB_TEAM", "Synthetic team");
        }
        if matches!(case, "bad-config" | "valid") {
            let config = temp.path().join(".config/teams-lab");
            std::fs::create_dir_all(&config).unwrap();
            let text = if case == "valid" {
                ["a", "b", "c"]
                    .map(|alias| format!("[[accounts]]\nalias='{alias}'\nupn='synthetic-{alias}@example.invalid'\n"))
                    .join("\n")
            } else {
                "invalid TOML = [".into()
            };
            std::fs::write(config.join("lab.toml"), text).unwrap();
        }
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn writing_settings_child() {
    let Ok(case) = std::env::var("CHANNEL_SETTINGS_CASE") else {
        return;
    };
    match case.as_str() {
        "disabled" => assert!(writing().unwrap().is_none()),
        "valid" => {
            let settings = writing().unwrap().unwrap();
            assert_eq!(settings.team, "Synthetic team");
            assert_eq!(settings.member, "synthetic-b@example.invalid");
        }
        "missing-team" | "blank-team" => assert!(writing().err().unwrap().to_string().contains("needs PROTO_LAB_TEAM")),
        _ => assert!(writing().is_err()),
    }
}

#[tokio::test]
async fn interrupted_event_stream_reports_missing_writes_without_claiming_a_success() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = Page::new(|command| {
        Ok(if command["method"] == "Runtime.evaluate" {
            evaluated(json!(true))
        } else {
            json!({})
        })
    })
    .await;
    let session = page.session(temp.path(), false);
    let (replacement, _) = tokio::sync::mpsc::unbounded_channel();
    drop(std::mem::replace(&mut page.events, replacement));
    page.driver.omit_observation_pauses();
    let mut report = ScenarioReport::new(ScenarioName::ChannelCreate);
    press_create(&mut page.driver, &mut report, 0).await.unwrap();
    assert_eq!(report.failures.len(), 1);
    assert!(created(&mut page.driver, &mut report, 0, CHANNEL_URL).await.is_err());
    add_member(&session, &mut page.driver, &mut report, 2, "peer")
        .await
        .unwrap();
    assert_eq!(report.failures.len(), 2);
    people_search(&session, &mut page.driver, &mut report).await.unwrap();
    assert!(report.notes.iter().any(|note| note.contains("no people search:")));
}
