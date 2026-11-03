//! Offline scenario judgments and orchestration against a synthetic CDP peer.

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::test_targets;
use crate::scenario::test_page::{Page, evaluated};
use crate::session::FenceReport;

fn plan(step: CallStep) -> CallPlan {
    CallPlan {
        scenario: ScenarioName::AbCall,
        chat_link: "https://teams.microsoft.com/l/chat/synthetic".to_owned(),
        call: RunCall {
            caller: "8:orgid:00000000-0000-4000-8000-000000000001".to_owned(),
            group_chat: None,
        },
        callees: vec![(Alias::B, "8:orgid:00000000-0000-4000-8000-000000000002".to_owned())],
        held_starts: &[Modality::Audio, Modality::Video],
        step,
        options: RunOptions {
            display: Display::Headless,
            load_timeout: Duration::from_millis(10),
            call: step,
        },
        pauses: CallPauses {
            page_steady: Duration::ZERO,
            calling_warm: Duration::ZERO,
            before_cancel: Duration::ZERO,
            unanswered: Duration::ZERO,
            after_action: Duration::ZERO,
            after_ring: Duration::ZERO,
            video_for: Duration::ZERO,
            video_wait: Duration::ZERO,
            keep_alive_hold: Duration::ZERO,
            hold_check: Duration::ZERO,
            dropped_for: Duration::ZERO,
            after_drop: Duration::ZERO,
        },
    }
}

fn verdict(method: &str, url: &str, decision: &str) -> String {
    json!({"type": "verdict", "channel": "http", "method": method, "url": url, "verdict": decision}).to_string()
}

#[tokio::test]
async fn invalid_call_plans_fail_before_opening_any_browser() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().to_path_buf());
    let error = run_plan(&paths, &plan(CallStep::Place)).await.unwrap_err();
    assert!(error.to_string().contains("only ab-call-app does"));
    let plan = plan(CallStep::Answer);
    assert_eq!(
        plan.callee_mri(Alias::B),
        "8:orgid:00000000-0000-4000-8000-000000000002"
    );
    assert_eq!(plan.callee_mri(Alias::C), "");
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    let error = plan.drive(&mut [], &mut None, &mut report).await.unwrap_err();
    assert!(error.to_string().contains("no caller's browser"));
    assert!(
        open_all(&paths, &[], &SessionOptions::fail_closed())
            .await
            .unwrap()
            .is_empty()
    );
    close_all(Vec::new(), &plan, None, &mut report).await;
    diagnose(&[], &mut report).await;
    assert!(report.failures.is_empty());
}

#[tokio::test]
async fn a_normal_call_plan_without_the_pinned_browser_fails_before_opening_a_lab_fence() {
    for step in [CallStep::Answer, CallStep::StartHeld] {
        let dir = tempfile::tempdir().unwrap();
        let paths = LabPaths::new(dir.path().to_path_buf());
        let error = run_plan(&paths, &plan(step)).await.unwrap_err();
        assert!(error.to_string().contains("Chrome"), "{error}");
        assert!(!paths.profile_dir(Alias::A).exists());
    }
}

#[test]
fn missing_account_mris_fail_with_the_account_name() {
    let dir = tempfile::tempdir().unwrap();
    let text = ["a", "b", "c"]
        .map(|alias| format!("[[accounts]]\nalias = '{alias}'\nupn = 'synthetic-{alias}@lab.example.com'\n"))
        .join("\n");
    let config = LabConfig::parse(&text, dir.path()).unwrap();
    let error = mri(&config, Alias::B).unwrap_err();
    assert!(error.to_string().contains("no MRI for b"));
    let text = text.replacen(
        "alias = 'b'",
        "alias = 'b'\nmri = '8:orgid:00000000-0000-4000-8000-000000000002'",
        1,
    );
    let config = LabConfig::parse(&text, dir.path()).unwrap();
    assert_eq!(
        mri(&config, Alias::B).unwrap(),
        "8:orgid:00000000-0000-4000-8000-000000000002"
    );
}

#[test]
fn caller_judgment_requires_a_start_and_rejects_permitted_starts_in_held_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    judge_caller(dir.path(), CallStep::Answer, &mut report);
    assert!(report.failures[0].contains("can't read a's capture"));
    for (step, text, failure) in [
        (CallStep::Answer, String::new(), Some("allowed no call start")),
        (CallStep::StartHeld, String::new(), None),
        (CallStep::Answer, verdict("POST", CALL_START_URL, "allow"), None),
        (
            CallStep::StartHeld,
            verdict("POST", CALL_START_URL, "allow"),
            Some("through in start-held"),
        ),
    ] {
        std::fs::write(dir.path().join(EVENTS_FILE), text).unwrap();
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        judge_caller(dir.path(), step, &mut report);
        assert_eq!(report.notes.len(), 1);
        match failure {
            Some(expected) => assert!(report.failures[0].contains(expected)),
            None => assert!(report.failures.is_empty(), "{:?}", report.failures),
        }
    }
}

#[test]
fn a_callee_report_requires_an_invitation_and_the_action_its_role_sent() {
    let dir = tempfile::tempdir().unwrap();
    let call = plan(CallStep::Answer).call;
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    judge_callee(
        (
            Alias::B,
            "8:orgid:00000000-0000-4000-8000-000000000002",
            CalleeRole::Other,
        ),
        &call,
        None,
        dir.path(),
        &mut report,
    );
    assert!(report.failures[0].contains("can't read b's capture"));
    for (role, text, expected_failures) in [
        (CalleeRole::Other, String::new(), 1),
        (CalleeRole::Declines, String::new(), 2),
        (
            CalleeRole::Declines,
            verdict(
                "DELETE",
                "https://api.flightproxy.teams.microsoft.com/api/v2/ep/synthetic/cc/v1/incoming/call/1/reject",
                "allow",
            ),
            1,
        ),
    ] {
        std::fs::write(dir.path().join(EVENTS_FILE), text).unwrap();
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        judge_callee(
            (Alias::B, "8:orgid:00000000-0000-4000-8000-000000000002", role),
            &call,
            Some(1),
            dir.path(),
            &mut report,
        );
        assert_eq!(report.failures.len(), expected_failures, "{:?}", report.failures);
        assert!(report.failures[0].contains("no invitation of the run's call"));
        assert!(report.notes[0].contains("0 invitation delivery(ies)"));
    }
}

#[test]
fn held_mode_requires_the_callers_forced_deny_and_reports_each_accounts_held_starts() {
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    report.accounts.push(super::super::AccountRun {
        alias: Alias::A,
        fence: FenceReport::default(),
        denials: Vec::new(),
        frame_denials: Vec::new(),
        capture: None,
    });
    judge_held(CallStep::StartHeld, &mut report);
    assert_eq!(report.failures, ["a's fence held back no call start"]);
    report.failures.clear();
    report.accounts[0].denials.push(Denial {
        method: "POST".to_owned(),
        url: CALL_START_URL.to_owned(),
        cause: DenyCause::Forced,
        label: Some(START_HELD.to_owned()),
        network_id: None,
    });
    judge_held(CallStep::StartHeld, &mut report);
    assert!(report.failures.is_empty());
    assert!(report.notes[0].contains("1 request(s)"));
    judge_held(CallStep::Answer, &mut report);
    assert!(report.failures.is_empty());
    let at = now_ms();
    assert!(at > 0);
    assert!(unix_ms() >= u128::from(at));
}

#[tokio::test]
async fn returning_to_chat_clicks_the_app_before_waiting_for_the_header() {
    let mut page = Page::new(|_| Ok(evaluated(json!(true)))).await;
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    back_to_chat(Alias::B, &mut page.driver, &mut report).await.unwrap();
    let commands = page.commands();
    assert_eq!(commands.len(), 2);
    assert!(
        commands[0]["params"]["expression"]
            .as_str()
            .unwrap()
            .contains("e.click()")
    );
    assert!(
        commands[1]["params"]["expression"]
            .as_str()
            .unwrap()
            .contains("chat-calling-meeting-buttons")
    );
    assert_eq!(report.notes, ["b: back at the chat"]);

    let mut page = Page::new(|_| Ok(evaluated(json!(false)))).await;
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    assert!(back_to_chat(Alias::B, &mut page.driver, &mut report).await.is_err());
    assert_eq!(page.commands().len(), 1);
    assert!(report.notes.is_empty());
}

async fn evaluation_page(values: Vec<Value>) -> Page {
    evaluation_page_in(values, &test_targets::chat()).await
}

async fn evaluation_page_in(values: Vec<Value>, chat: &str) -> Page {
    let chat = chat.to_owned();
    let mut values = std::collections::VecDeque::from(values);
    Page::new(move |command| {
        assert_eq!(command["method"], "Runtime.evaluate");
        if command["params"]["expression"]
            .as_str()
            .unwrap()
            .contains("return buttons.length === 1")
        {
            return Ok(evaluated(json!(chat)));
        }
        Ok(evaluated(
            values.pop_front().expect("an unexpected extra page evaluation"),
        ))
    })
    .await
}

#[tokio::test]
async fn the_synthetic_session_uses_the_real_fail_closed_fence() {
    let dir = tempfile::tempdir().unwrap();
    let mut page = Page::new(|command| {
        assert_eq!(command["method"], "Fetch.failRequest");
        assert_eq!(command["params"]["requestId"], "synthetic-write");
        assert_eq!(command["params"]["errorReason"], "BlockedByClient");
        Ok(json!({}))
    })
    .await;
    let mut session = test_targets::allow(page.session(dir.path(), true), test_targets::A);
    page.send(
        "Fetch.requestPaused",
        json!({
            "requestId": "synthetic-write", "networkId": "synthetic-network", "resourceType": "XHR",
            "request": {"method": "POST", "url": "https://graph.microsoft.com/v1.0/me/sendMail",
                        "headers": {}, "postData": "{}"},
        }),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while session.denials().is_empty() || page.commands().is_empty() {
        assert!(tokio::time::Instant::now() < deadline);
        session.pump(Duration::from_millis(10)).await;
    }
    assert_eq!(session.denials().len(), 1);
    assert_eq!(session.report().denied_requests.get("graph.microsoft.com"), Some(&1));
    assert_eq!(page.commands().len(), 1);
    assert!(session.capture_dir().unwrap().join(EVENTS_FILE).is_file());
}

#[tokio::test]
async fn placing_a_call_uses_the_shown_button_or_opens_its_options_before_confirmation() {
    for (shown, modality) in [(true, Modality::Audio), (false, Modality::Video)] {
        let values = if shown {
            vec![json!(true); 5]
        } else {
            vec![
                json!(true),
                json!(false),
                json!(true),
                json!(true),
                json!(true),
                json!(true),
                json!(true),
            ]
        };
        let mut page = evaluation_page_in(values, "19:synthetic-target@thread.v2").await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = test_targets::allow(page.session(dir.path(), false), test_targets::A);
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        place_call(
            (Alias::A, &mut session),
            "19:synthetic-target@thread.v2",
            modality,
            &mut report,
        )
        .await
        .unwrap();
        let commands = page.commands();
        let expressions: Vec<_> = commands
            .iter()
            .map(|command| command["params"]["expression"].as_str().unwrap())
            .filter(|expression| !expression.contains("return buttons.length === 1"))
            .collect();
        assert!(expressions[0].contains("data-track-thread-id"));
        assert!(expressions[1].contains(&js_string(modality.control())));
        if !shown {
            assert!(expressions[2].contains("dropdown-calling-toggle-more-options-btn"));
            assert!(expressions[2].contains("e.click()"));
            assert!(expressions[3].contains(&js_string(modality.control())));
        }
        assert!(expressions[expressions.len() - 3].contains(&js_string(modality.control())));
        assert!(expressions[expressions.len() - 3].contains("e.click()"));
        assert!(
            expressions
                .last()
                .unwrap()
                .contains("group-start-call-confirmation-btn")
        );
        assert_eq!(report.notes.len(), 2);
        assert!(report.notes[0].contains(modality.what()));
        assert!(report.notes[1].contains("confirmed the group call"));
    }
}

#[tokio::test]
async fn a_restored_unrelated_chat_is_refused_before_any_call_control_is_touched() {
    let mut page = evaluation_page_in(vec![json!(false)], "19:synthetic-target@thread.v2").await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = test_targets::allow(page.session(dir.path(), false), test_targets::A);
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    let error = place_call(
        (Alias::A, &mut session),
        "19:synthetic-target@thread.v2",
        Modality::Audio,
        &mut report,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not show exactly the planned conversation")
    );
    assert!(report.notes.is_empty());
    let commands = page.commands();
    assert_eq!(commands.len(), 1);
    let expression = commands[0]["params"]["expression"].as_str().unwrap();
    assert!(expression.contains("19:synthetic-target@thread.v2"));
    assert!(expression.contains("sends.length !== 1"));
    assert!(!expression.contains("click()"));
}

#[tokio::test]
async fn a_selection_changed_before_the_call_or_confirmation_is_refused_atomically() {
    for values in [
        vec![json!(true), json!(true), json!(false)],
        vec![json!(true), json!(true), json!(true), json!(true), json!(false)],
    ] {
        let mut page = evaluation_page_in(values, "19:synthetic-target@thread.v2").await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = test_targets::allow(page.session(dir.path(), false), test_targets::A);
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        let error = place_call(
            (Alias::A, &mut session),
            "19:synthetic-target@thread.v2",
            Modality::Audio,
            &mut report,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("conversation or its call control changed"));
        let commands = page.commands();
        let expression = commands.last().unwrap()["params"]["expression"].as_str().unwrap();
        assert!(expression.contains("data-track-thread-id"));
        assert!(expression.contains("19:synthetic-target@thread.v2"));
        assert!(expression.find("return false").unwrap() < expression.find("e.click()").unwrap());
    }
}

#[test]
fn planned_conversations_are_derived_from_the_exact_identities_or_group_thread() {
    let mut call = plan(CallStep::Answer);
    assert_eq!(
        call.expected_chat().unwrap(),
        "19:00000000-0000-4000-8000-000000000001_00000000-0000-4000-8000-000000000002@unq.gbl.spaces"
    );
    call.callees.clear();
    assert!(call.expected_chat().is_err());
    call.call.group_chat = Some("19:synthetic-group@thread.v2".to_owned());
    assert_eq!(call.expected_chat().unwrap(), "19:synthetic-group@thread.v2");
    assert!(call_chat("unknown", "8:orgid:unknown").is_err());
}

#[tokio::test]
async fn a_call_without_verified_own_identity_or_with_a_non_lab_member_never_clicks_call() {
    for outsider in [false, true] {
        let mut page = evaluation_page(vec![json!(true), json!(true)]).await;
        let dir = tempfile::tempdir().unwrap();
        let session = page.session(dir.path(), false);
        let mut session = if outsider {
            test_targets::with_members(
                session,
                test_targets::A,
                &[test_targets::A, "8:orgid:00000000-0000-4000-8000-000000000004"],
            )
        } else {
            session
        };
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        let error = place_call(
            (Alias::A, &mut session),
            &test_targets::chat(),
            Modality::Audio,
            &mut report,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains(if outsider {
            "server did not return the exact target's full membership"
        } else {
            "no explicit lab action target context"
        }));
        assert!(report.notes.is_empty());
        assert!(
            page.commands()
                .iter()
                .all(|command| { !command["params"]["expression"].as_str().unwrap().contains("e.click()") })
        );
    }
}

#[tokio::test]
async fn retry_and_hangup_buttons_are_clicked_only_when_the_page_shows_them() {
    for shown in [true, false] {
        let values = if shown {
            vec![json!(true); 4]
        } else {
            vec![json!(false); 2]
        };
        let mut page = evaluation_page(values).await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = test_targets::allow(page.session(dir.path(), false), test_targets::A);
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        give_up_retry(&mut session, &mut report).await.unwrap();
        hang_up(&mut session, &mut report).await.unwrap();
        assert_eq!(page.commands().len(), if shown { 4 } else { 2 });
        if shown {
            assert!(report.notes[0].contains("cancelled the retry screen"));
            assert!(report.notes[1].contains("hung up at"));
        } else {
            assert_eq!(report.notes, ["a: the call screen was gone: nothing to hang up"]);
        }
    }
}

#[tokio::test]
async fn an_error_reading_the_call_screen_is_reported_without_hanging_up() {
    let mut page = Page::new(|command| {
        assert_eq!(command["method"], "Runtime.evaluate");
        Err("synthetic renderer gone".to_owned())
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = test_targets::allow(page.session(dir.path(), false), test_targets::A);
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    hang_up_after_failure(Alias::B, &mut session, &mut report).await;
    assert_eq!(page.commands().len(), 1);
    assert!(report.notes[0].contains("couldn't hang up after the failure"));
    assert!(report.notes[0].contains("synthetic renderer gone"));
}

#[tokio::test]
async fn call_diagnostics_include_controls_and_microphone_without_changing_the_page() {
    let mut page = evaluation_page(vec![
        json!(true),
        json!("Mute"),
        json!(["hangup-main-btn"]),
        json!([]),
        json!("{synthetic:1}"),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let session = test_targets::allow(page.session(dir.path(), false), test_targets::A);
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    watch_controls(Alias::B, &session).await.unwrap();
    note_microphone(Alias::B, &session, &mut report).await;
    let controls = call_controls(Alias::B, &session).await.unwrap();
    assert!(controls.contains("hangup-main-btn"));
    let callees = [(Alias::B, session)];
    note_ring_controls(&callees, &mut report).await;
    assert!(report.notes[0].contains("Mute"));
    assert!(report.notes[1].contains("{synthetic:1}"));
    let commands = page.commands();
    assert_eq!(commands.len(), 5);
    assert_eq!(commands[0]["params"]["expression"], WATCH_CONTROLS);
    assert!(
        commands
            .iter()
            .all(|command| !command["params"]["expression"].as_str().unwrap().contains("e.click()"))
    );
}

#[tokio::test]
async fn ring_end_reports_both_a_gone_and_a_still_visible_notification() {
    for gone in [true, false] {
        let mut page = Page::new(move |command| {
            assert_eq!(command["method"], "Runtime.evaluate");
            Ok(evaluated(json!(gone)))
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let mut callees = [(
            Alias::B,
            test_targets::allow(page.session(dir.path(), false), test_targets::A),
        )];
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        wait_ring_end(&mut callees, Duration::from_secs(1), &mut report).await;
        assert!(
            !page.commands().is_empty(),
            "the actual page predicate must be evaluated"
        );
        assert!(report.notes[0].contains(if gone {
            "incoming call went"
        } else {
            "incoming call stayed"
        }));
    }
}

#[tokio::test]
async fn closing_a_calling_socket_releases_its_object_group_and_counts_only_closed_sockets() {
    let mut page = Page::new(|command| match command["method"].as_str().unwrap() {
        "Runtime.evaluate" => {
            assert_eq!(command["params"]["expression"], "WebSocket.prototype");
            Ok(json!({"result": {"objectId": "prototype"}}))
        }
        "Runtime.queryObjects" => {
            assert_eq!(command["params"]["prototypeObjectId"], "prototype");
            Ok(json!({"objects": {"objectId": "sockets"}}))
        }
        "Runtime.callFunctionOn" => {
            assert_eq!(command["params"]["objectId"], "sockets");
            assert!(
                command["params"]["functionDeclaration"]
                    .as_str()
                    .unwrap()
                    .contains("SkypeSpaces")
            );
            Ok(evaluated(json!(2)))
        }
        "Runtime.releaseObjectGroup" => {
            assert_eq!(command["params"]["objectGroup"], "lab-dropped");
            Ok(json!({}))
        }
        method => Err(format!("unexpected synthetic command {method}")),
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let session = test_targets::allow(page.session(dir.path(), false), test_targets::A);
    assert_eq!(close_calling_socket(&session).await, 2);
    assert_eq!(
        page.commands()
            .iter()
            .map(|command| command["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "Runtime.evaluate",
            "Runtime.queryObjects",
            "Runtime.callFunctionOn",
            "Runtime.releaseObjectGroup"
        ]
    );
}

fn invitation(call: &RunCall, callee: &str) -> String {
    let mut payload = json!({
        "callNotification": {"from": {"id": call.caller}, "to": {"id": callee}},
        "conversationInvitation": {"conversationController": "https://synthetic.invalid/conv/call"},
    });
    if let Some(chat) = &call.group_chat {
        payload["groupChat"] = json!({"threadId": chat});
    }
    let gp = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
    let body = json!({"evt": 107, "gp": gp}).to_string();
    let message = json!({"id": 1, "method": "POST", "url": "/v4/f/synthetic/", "body": body});
    json!({"type": "cdp", "ts_ms": 101, "method": "Network.webSocketFrameReceived",
           "params": {"response": {"payloadData": format!("3:::{message}")}}})
    .to_string()
}

#[tokio::test]
async fn a_ring_click_requires_a_bound_invitation_and_exactly_one_shown_call() {
    for shown in [true, false] {
        let mut page = evaluation_page(vec![json!(shown)]).await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = test_targets::allow(page.session(dir.path(), true), test_targets::A);
        let plan = plan(CallStep::Answer);
        let callee = plan.callee_mri(Alias::B);
        std::fs::write(
            session.capture_dir().unwrap().join(EVENTS_FILE),
            invitation(&plan.call, callee),
        )
        .unwrap();
        let action = plan.step.callee_action().unwrap();
        let outcome = check_ring(
            (Alias::B, &mut session),
            action,
            (&plan.call, callee),
            100,
            EarlierRings::Judged,
        )
        .await;
        assert_eq!(outcome.is_ok(), shown);
        if let Err(error) = outcome {
            assert!(error.to_string().contains("doesn't show exactly one incoming call"));
        }
        assert_eq!(page.commands().len(), 1);
        let expression = page.commands()[0]["params"]["expression"].as_str().unwrap().to_owned();
        assert!(expression.contains(".length <= 1"));
        assert!(expression.contains(".length === 1"));
        assert!(!expression.contains("e.click()"));
    }
}

#[tokio::test]
async fn an_unrecorded_or_unrelated_ring_is_refused_before_any_page_action() {
    for record in [true, false] {
        let mut page = Page::new(|_| Err("no CDP command is permitted for this refusal".to_owned())).await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = test_targets::allow(page.session(dir.path(), record), test_targets::A);
        let plan = plan(CallStep::Answer);
        let callee = plan.callee_mri(Alias::B);
        if record {
            std::fs::write(
                session.capture_dir().unwrap().join(EVENTS_FILE),
                invitation(&plan.call, "8:orgid:outsider"),
            )
            .unwrap();
        }
        let outcome = check_ring(
            (Alias::B, &mut session),
            plan.step.callee_action().unwrap(),
            (&plan.call, callee),
            100,
            EarlierRings::Judged,
        )
        .await;
        assert!(outcome.is_err());
        assert!(outcome.unwrap_err().to_string().contains("doesn't click answer"));
        assert!(page.commands().is_empty());
    }
}

#[tokio::test]
async fn joining_requires_the_group_invitation_and_the_groups_visible_chat() {
    for shown in [true, false] {
        let reading = if shown {
            json!({"sends": 1, "ours": 1, "joins": 1})
        } else {
            json!({"sends": 1, "ours": 0, "joins": 1})
        };
        let mut page = evaluation_page(vec![json!(reading.to_string())]).await;
        let dir = tempfile::tempdir().unwrap();
        let session = test_targets::allow(page.session(dir.path(), true), test_targets::A);
        let mut plan = plan(CallStep::Join);
        plan.call.group_chat = Some("19:synthetic@thread.v2".to_owned());
        std::fs::write(
            session.capture_dir().unwrap().join(EVENTS_FILE),
            invitation(&plan.call, plan.callee_mri(Alias::B)),
        )
        .unwrap();
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        let outcome = plan.check_join(Alias::B, &session, 100, &mut report).await;
        assert_eq!(outcome.is_ok(), shown);
        assert_eq!(report.notes.len(), usize::from(shown));
        assert_eq!(page.commands().len(), 1);
        assert!(
            page.commands()[0]["params"]["expression"]
                .as_str()
                .unwrap()
                .contains("19:synthetic@thread.v2")
        );
    }
}

#[tokio::test]
async fn joining_without_group_capture_or_invitation_never_evaluates_the_page() {
    let mut page = Page::new(|_| Err("no CDP command is permitted for this refusal".to_owned())).await;
    let dir = tempfile::tempdir().unwrap();
    let session = test_targets::allow(page.session(dir.path(), false), test_targets::A);
    let mut plan = plan(CallStep::Join);
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    let error = plan.check_join(Alias::B, &session, 100, &mut report).await.unwrap_err();
    assert!(error.to_string().contains("calls no group chat"));
    plan.call.group_chat = Some("19:synthetic@thread.v2".to_owned());
    let error = plan.check_join(Alias::B, &session, 100, &mut report).await.unwrap_err();
    assert!(error.to_string().contains("records no capture"));
    assert!(page.commands().is_empty());

    let mut page = Page::new(|_| Err("no CDP command is permitted for this refusal".to_owned())).await;
    let session = test_targets::allow(page.session(dir.path(), true), test_targets::A);
    let error = plan.check_join(Alias::B, &session, 100, &mut report).await.unwrap_err();
    assert!(error.to_string().contains("no invitation of the run's call"));
    assert!(page.commands().is_empty());
    plan.join_late(100, &mut [], &mut report).await.unwrap();
}

#[test]
fn a_callee_with_its_invitation_still_requires_an_allowed_attach() {
    let dir = tempfile::tempdir().unwrap();
    let plan = plan(CallStep::Answer);
    let callee = plan.callee_mri(Alias::B);
    let own = invitation(&plan.call, callee);
    let file = dir.path().join(EVENTS_FILE);
    std::fs::write(&file, &own).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    judge_callee(
        (Alias::B, callee, CalleeRole::Other),
        &plan.call,
        Some(100),
        dir.path(),
        &mut report,
    );
    assert!(report.failures[0].contains("let no attach"));
    std::fs::write(
        &file,
        format!(
            "{own}\n{}",
            verdict(
                "POST",
                "https://api.flightproxy.teams.microsoft.com/api/v2/ep/synthetic/cc/v1/forked/call/1/attach",
                "allow"
            )
        ),
    )
    .unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    judge_callee(
        (Alias::B, callee, CalleeRole::Other),
        &plan.call,
        Some(100),
        dir.path(),
        &mut report,
    );
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert!(report.notes[0].contains("1 invitation delivery(ies)"));
}

#[test]
fn live_recording_pauses_keep_the_existing_scenario_durations() {
    let pauses = CallPauses::default();
    assert_eq!(pauses.before_cancel, RING_BEFORE_CANCEL);
    assert_eq!(pauses.unanswered, UNANSWERED);
    assert_eq!(pauses.after_action, AFTER_ACTION);
    assert_eq!(pauses.after_ring, AFTER_RING);
    assert_eq!(
        pauses,
        CallPauses {
            page_steady: Duration::from_secs(8),
            calling_warm: Duration::from_secs(15),
            before_cancel: Duration::from_secs(8),
            unanswered: Duration::from_secs(50),
            after_action: Duration::from_secs(12),
            after_ring: Duration::from_secs(8),
            video_for: Duration::from_secs(15),
            video_wait: Duration::from_secs(45),
            keep_alive_hold: Duration::from_mins(47),
            hold_check: Duration::from_secs(60),
            dropped_for: Duration::from_secs(10),
            after_drop: Duration::from_secs(25),
        }
    );
}

#[tokio::test]
async fn cancel_and_unanswered_steps_place_the_call_then_hang_up_and_record_controls() {
    for step in [CallStep::Cancel, CallStep::Timeout] {
        let mut origin_peer = evaluation_page(vec![json!(true); 7]).await;
        let mut answer_peer = evaluation_page(vec![json!("{synthetic:1}")]).await;
        let origin_dir = tempfile::tempdir().unwrap();
        let answer_dir = tempfile::tempdir().unwrap();
        let mut caller = test_targets::allow(origin_peer.session(origin_dir.path(), true), test_targets::A);
        let mut callees = [(
            Alias::B,
            test_targets::allow(answer_peer.session(answer_dir.path(), true), test_targets::A),
        )];
        let plan = plan(step);
        let mut placed = None;
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        plan.ring(&mut caller, &mut callees, &mut placed, &mut report)
            .await
            .unwrap();
        assert!(placed.is_some_and(|at| at > 0));
        assert!(report.failures.is_empty());
        assert_eq!(origin_peer.commands().len(), 11);
        assert_eq!(answer_peer.commands().len(), 1);
        assert!(report.notes.iter().any(|note| note.contains("hung up at")));
        assert!(
            report
                .notes
                .last()
                .unwrap()
                .contains("controls shown since the call was placed")
        );
    }
}

#[tokio::test]
async fn declining_checks_the_bound_ring_before_clicking_then_waits_for_it_to_end() {
    let mut origin_peer = evaluation_page(vec![json!(true); 7]).await;
    let mut answer_peer = evaluation_page(vec![json!(true), json!(true), json!(true), json!(true), json!("{}")]).await;
    let origin_dir = tempfile::tempdir().unwrap();
    let answer_dir = tempfile::tempdir().unwrap();
    let mut caller = test_targets::allow(origin_peer.session(origin_dir.path(), true), test_targets::A);
    let recipient = test_targets::allow(answer_peer.session(answer_dir.path(), true), test_targets::A);
    let plan = plan(CallStep::Decline);
    let mut capture: Value = serde_json::from_str(&invitation(&plan.call, plan.callee_mri(Alias::B))).unwrap();
    capture["ts_ms"] = json!(now_ms() + 60_000);
    std::fs::write(recipient.capture_dir().unwrap().join(EVENTS_FILE), capture.to_string()).unwrap();
    let mut callees = [(Alias::B, recipient)];
    let mut placed = None;
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    plan.ring(&mut caller, &mut callees, &mut placed, &mut report)
        .await
        .unwrap();
    assert!(report.failures.is_empty());
    assert!(report.notes.iter().any(|note| note.contains("clicked decline")));
    assert!(report.notes.iter().any(|note| note.contains("incoming call went")));
    let commands = answer_peer.commands();
    assert_eq!(commands.len(), 5);
    let expression = |index: usize| commands[index]["params"]["expression"].as_str().unwrap();
    assert!(expression(0).contains(" !== null"));
    assert!(expression(1).contains(".length === 1"));
    assert!(!expression(1).contains("e.click()"));
    assert!(expression(2).contains("Decline call"));
    assert!(expression(2).contains("e.click()"));
    assert!(expression(3).starts_with("!("));
}

async fn calling_page() -> Page {
    calling_page_in(&test_targets::chat()).await
}

async fn calling_page_in(chat: &str) -> Page {
    let chat = chat.to_owned();
    let relay: std::sync::Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>> =
        std::sync::Arc::default();
    let callback = std::sync::Arc::clone(&relay);
    let page = Page::new(move |command| match command["method"].as_str().unwrap() {
        "Page.navigate" => {
            assert!(
                command["params"]["url"]
                    .as_str()
                    .unwrap()
                    .starts_with("https://teams.microsoft.com/")
            );
            Ok(json!({}))
        }
        "Emulation.setDeviceMetricsOverride" => {
            assert!(command["params"]["width"].as_u64().unwrap() > 0);
            assert!(command["params"]["height"].as_u64().unwrap() > 0);
            Ok(json!({}))
        }
        "Runtime.evaluate" => {
            let expression = command["params"]["expression"].as_str().unwrap();
            if expression.contains("return buttons.length === 1") {
                return Ok(evaluated(json!(chat)));
            }
            if expression == "WebSocket.prototype" {
                return Ok(json!({"result": {"objectId": "synthetic-prototype"}}));
            }
            if expression.contains("sends.length !== 1") {
                return Ok(evaluated(json!(true)));
            }
            if expression.contains("const sends =") {
                return Ok(evaluated(json!(json!({"sends": 1, "ours": 1, "joins": 1}).to_string())));
            }
            Ok(evaluated(json!(true)))
        }
        "Runtime.queryObjects" => Ok(json!({"objects": {"objectId": "synthetic-sockets"}})),
        "Runtime.callFunctionOn" => {
            assert_eq!(command["params"]["objectId"], "synthetic-sockets");
            callback
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .send(json!({
                    "sessionId": "synthetic-page", "method": "Network.webSocketClosed",
                    "params": {"requestId": "synthetic-calling"},
                }))
                .unwrap();
            Ok(evaluated(json!(1)))
        }
        "Runtime.releaseObjectGroup" | "Browser.close" => Ok(json!({})),
        "Network.emulateNetworkConditions" => {
            assert!(command["params"]["offline"].is_boolean());
            assert_eq!(command["params"]["latency"], 0);
            Ok(json!({}))
        }
        "Page.captureScreenshot" => Ok(
            json!({"data": base64::engine::general_purpose::STANDARD.encode(crate::scenario::ab_image::test_png(3))}),
        ),
        method => Err(format!("unexpected command to synthetic calling page: {method}")),
    })
    .await;
    relay.lock().unwrap().replace(page.sender());
    page
}

fn connected_socket() -> String {
    let connected = json!({"name": "trouter.connected", "args": []});
    [
        json!({"type": "cdp", "method": "Network.webSocketCreated", "params": {"requestId": "synthetic-calling", "url": "wss://go.trouter.teams.microsoft.com/v4/c?tc=%7B%22ua%22:%22SkypeSpaces%22%7D"}}),
        json!({"type": "cdp", "method": "Network.webSocketFrameReceived", "params": {"requestId": "synthetic-calling", "response": {"payloadData": format!("5:1::{connected}")}}}),
    ].map(|entry| entry.to_string()).join("\n")
}

#[tokio::test]
async fn a_decline_drive_loads_both_pages_and_unconfirmed_closes_preserve_both_reports() {
    let mut origin_peer = calling_page().await;
    let mut answer_peer = calling_page().await;
    let origin_dir = tempfile::tempdir().unwrap();
    let answer_dir = tempfile::tempdir().unwrap();
    let origin = test_targets::allow(
        origin_peer.loaded_session(origin_dir.path(), true).await,
        test_targets::A,
    );
    let recipient = test_targets::allow(
        answer_peer.loaded_session(answer_dir.path(), true).await,
        test_targets::A,
    );
    let plan = plan(CallStep::Decline);
    let mut received: Value = serde_json::from_str(&invitation(&plan.call, plan.callee_mri(Alias::B))).unwrap();
    received["ts_ms"] = json!(now_ms() + 60_000);
    std::fs::write(
        origin.capture_dir().unwrap().join(EVENTS_FILE),
        format!("{}\n{}\n", connected_socket(), verdict("POST", CALL_START_URL, "allow")),
    )
    .unwrap();
    let attach = verdict(
        "POST",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/synthetic/cc/v1/forked/call/1/attach",
        "allow",
    );
    let reject = verdict(
        "DELETE",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/synthetic/cc/v1/incoming/call/1/reject",
        "allow",
    );
    std::fs::write(
        recipient.capture_dir().unwrap().join(EVENTS_FILE),
        format!("{}\n{received}\n{attach}\n{reject}\n", connected_socket()),
    )
    .unwrap();
    let mut sessions = [(Alias::A, origin), (Alias::B, recipient)];
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    let mut placed = None;
    plan.drive(&mut sessions, &mut placed, &mut report).await.unwrap();
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("a: the calling socket is up"))
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("b: the calling socket is up"))
    );
    assert!(report.notes.iter().any(|note| note.contains("clicked decline")));
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    close_all(Vec::from(sessions), &plan, placed, &mut report).await;
    assert_eq!(report.failures.len(), 2, "{:?}", report.failures);
    assert!(
        report
            .failures
            .iter()
            .all(|failure| failure.contains("the fence had lost its connection"))
    );
    assert_eq!(report.accounts.len(), 2);
    for commands in [origin_peer.commands(), answer_peer.commands()] {
        assert_eq!(commands.first().unwrap()["method"], "Page.navigate");
        assert_eq!(commands.last().unwrap()["method"], "Browser.close");
        let resize = commands
            .iter()
            .position(|command| command["method"] == "Emulation.setDeviceMetricsOverride")
            .unwrap();
        let first_click = commands
            .iter()
            .position(|command| {
                command["params"]["expression"]
                    .as_str()
                    .is_some_and(|expression| expression.contains("e.click()"))
            })
            .unwrap();
        assert!(resize < first_click);
    }
}

fn media_capture() -> String {
    let stats = json!({"type": "rtc", "payload": {"event": "stats", "data": [
        {"type": "outbound-rtp", "kind": "audio", "packetsSent": 40},
        {"type": "inbound-rtp", "kind": "audio", "packetsReceived": 30, "totalAudioEnergy": 0.5},
        {"type": "inbound-rtp", "kind": "video", "framesDecoded": 3},
    ]}})
    .to_string();
    let reading = json!({"type": "rtc", "session": "synthetic", "context": 1, "payload": {
        "event": "audio", "pc": 1, "data": {"track": "synthetic-track", "rms": 0.25, "tone": 0.99},
    }})
    .to_string();
    let keep_alive = verdict(
        "POST",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/synthetic/cc/v1/active/call/1/leg/1",
        "allow",
    );
    format!("{stats}\n{}{keep_alive}\n", format!("{reading}\n").repeat(10))
}

#[tokio::test]
async fn answered_video_and_keepalive_phases_require_audio_and_media_on_both_pages() {
    for step in [CallStep::Answer, CallStep::Video, CallStep::KeepAlive] {
        let mut origin_peer = calling_page().await;
        let mut answer_peer = calling_page().await;
        let origin_dir = tempfile::tempdir().unwrap();
        let answer_dir = tempfile::tempdir().unwrap();
        let mut origin = test_targets::allow(origin_peer.session(origin_dir.path(), true), test_targets::A);
        let recipient = test_targets::allow(answer_peer.session(answer_dir.path(), true), test_targets::A);
        for session in [&origin, &recipient] {
            std::fs::write(session.capture_dir().unwrap().join(EVENTS_FILE), media_capture()).unwrap();
        }
        let mut callees = [(Alias::B, recipient)];
        let mut report = ScenarioReport::new(ScenarioName::AbCall);
        converse(step, &mut origin, &mut callees, &mut report, plan(step).pauses)
            .await
            .unwrap();
        assert!(report.failures.is_empty(), "{step:?}: {:?}", report.failures);
        for alias in ["a", "b"] {
            assert!(
                report
                    .notes
                    .iter()
                    .any(|note| note.starts_with(&format!("{alias}: audio 40 packet(s) sent, 30 received")))
            );
            assert!(
                report
                    .notes
                    .iter()
                    .any(|note| note.starts_with(&format!("{alias}: heard the other side's tone")))
            );
            if step == CallStep::Video {
                assert!(
                    report
                        .notes
                        .iter()
                        .any(|note| note == &format!("{alias}: decoded 3 video frame(s)"))
                );
            }
            if step == CallStep::KeepAlive {
                assert!(
                    report
                        .notes
                        .iter()
                        .any(|note| note == &format!("{alias}: the fence let 1 keep-alive(s) through"))
                );
            }
        }
        let commands = answer_peer.commands();
        assert!(
            commands.last().unwrap()["params"]["expression"]
                .as_str()
                .unwrap()
                .contains("e.click()")
        );
        assert!(
            commands.last().unwrap()["params"]["expression"]
                .as_str()
                .unwrap()
                .contains("hangup")
        );
    }
}

#[tokio::test]
async fn absent_audio_video_and_keepalive_proof_fail_each_pages_result() {
    let mut origin_peer = calling_page().await;
    let mut answer_peer = calling_page().await;
    let origin_dir = tempfile::tempdir().unwrap();
    let answer_dir = tempfile::tempdir().unwrap();
    let mut origin = test_targets::allow(origin_peer.session(origin_dir.path(), true), test_targets::A);
    let mut callees = [(
        Alias::B,
        test_targets::allow(answer_peer.session(answer_dir.path(), true), test_targets::A),
    )];
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    judge_audio(&mut origin, &mut callees, &mut report);
    assert_eq!(report.failures.len(), 2);
    assert!(
        report
            .failures
            .iter()
            .all(|failure| failure.contains("audio didn't go both ways"))
    );
    report.failures.clear();
    let pauses = plan(CallStep::Video).pauses;
    show_video(&mut origin, &mut callees, &mut report, pauses)
        .await
        .unwrap();
    assert_eq!(report.failures.len(), 2);
    assert!(
        report
            .failures
            .iter()
            .all(|failure| failure.contains("decoded no video"))
    );
    report.failures.clear();
    keep_alive(&mut origin, &mut callees, &mut report, pauses).await;
    assert_eq!(report.failures.len(), 2);
    assert!(
        report
            .failures
            .iter()
            .all(|failure| failure.contains("let no keep-alive"))
    );
}

#[tokio::test]
async fn a_late_join_waits_for_the_ring_and_binds_the_group_before_clicking_join() {
    let mut first_peer = calling_page().await;
    let mut join_peer = calling_page_in("19:synthetic@thread.v2").await;
    let first_dir = tempfile::tempdir().unwrap();
    let join_dir = tempfile::tempdir().unwrap();
    let first = test_targets::allow(first_peer.session(first_dir.path(), true), test_targets::A);
    let joiner = test_targets::allow(join_peer.session(join_dir.path(), true), test_targets::A);
    let capture_dir = joiner.capture_dir().unwrap().to_path_buf();
    let mut plan = plan(CallStep::Join);
    plan.callees.push((Alias::C, "8:orgid:synthetic-c".to_owned()));
    plan.call.group_chat = Some("19:synthetic@thread.v2".to_owned());
    std::fs::write(
        capture_dir.join(EVENTS_FILE),
        invitation(&plan.call, plan.callee_mri(Alias::C)),
    )
    .unwrap();
    let mut callees = [(Alias::B, first), (Alias::C, joiner)];
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    plan.join_late(100, &mut callees, &mut report).await.unwrap();
    assert!(report.failures.is_empty());
    assert!(report.notes.iter().any(|note| note.contains("c: clicked join")));
    assert!(report.notes.iter().any(|note| note.contains("b: hung up")));
    assert!(capture_dir.join("joined.png").is_file());
    let commands = join_peer.commands();
    let binding = commands
        .iter()
        .position(|command| {
            command["params"]["expression"]
                .as_str()
                .is_some_and(|expression| expression.contains("const sends ="))
        })
        .unwrap();
    let click_at = commands
        .iter()
        .position(|command| {
            command["params"]["expression"].as_str().is_some_and(|expression| {
                expression.contains("e.click()") && expression.contains(&js_string(CHAT_JOIN_CSS))
            })
        })
        .unwrap();
    assert!(binding < click_at);
    assert_eq!(commands.last().unwrap()["method"], "Page.captureScreenshot");
}

#[tokio::test]
async fn dropping_a_page_closes_its_calling_socket_restores_network_and_checks_both_screens() {
    let mut origin_peer = calling_page().await;
    let mut answer_peer = calling_page().await;
    let origin_dir = tempfile::tempdir().unwrap();
    let answer_dir = tempfile::tempdir().unwrap();
    let mut origin = test_targets::allow(origin_peer.session(origin_dir.path(), true), test_targets::A);
    let recipient = test_targets::allow(answer_peer.session(answer_dir.path(), true), test_targets::A);
    std::fs::write(recipient.capture_dir().unwrap().join(EVENTS_FILE), connected_socket()).unwrap();
    let mut callees = [(Alias::B, recipient)];
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    drop_off(&mut origin, &mut callees, &mut report, plan(CallStep::Dropped).pauses)
        .await
        .unwrap();
    assert!(report.failures.is_empty());
    assert_eq!(
        report
            .notes
            .iter()
            .filter(|note| note.contains("call is still up"))
            .count(),
        2
    );
    let commands = answer_peer.commands();
    let networks: Vec<_> = commands
        .iter()
        .filter(|command| command["method"] == "Network.emulateNetworkConditions")
        .map(|command| command["params"]["offline"].as_bool().unwrap())
        .collect();
    assert_eq!(networks, [true, false]);
    assert!(report.notes.iter().any(|note| note.contains("closed 1 calling socket")));
    drop_off(&mut origin, &mut [], &mut report, plan(CallStep::Dropped).pauses)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_socket_close_failure_still_brings_the_page_back_online() {
    let mut origin_peer = calling_page().await;
    let mut answer_peer = Page::new(|command| match command["method"].as_str().unwrap() {
        "Network.emulateNetworkConditions" => Ok(json!({})),
        "Runtime.evaluate" => Err("synthetic socket inspection failed".to_owned()),
        method => Err(format!("unexpected command {method}")),
    })
    .await;
    let origin_dir = tempfile::tempdir().unwrap();
    let answer_dir = tempfile::tempdir().unwrap();
    let mut origin = test_targets::allow(origin_peer.session(origin_dir.path(), true), test_targets::A);
    let mut callees = [(
        Alias::B,
        test_targets::allow(answer_peer.session(answer_dir.path(), true), test_targets::A),
    )];
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    let error = drop_off(&mut origin, &mut callees, &mut report, plan(CallStep::Dropped).pauses)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("closed no calling socket"));
    let commands = answer_peer.commands();
    assert_eq!(commands.first().unwrap()["params"]["offline"], true);
    assert_eq!(commands.last().unwrap()["params"]["offline"], false);
}

#[tokio::test]
async fn a_call_ending_during_a_hold_fails_the_hold_and_requires_keepalive_proof() {
    let mut page = evaluation_page(vec![json!(false)]).await;
    let dir = tempfile::tempdir().unwrap();
    let mut session = test_targets::allow(page.session(dir.path(), true), test_targets::A);
    let mut pauses = plan(CallStep::KeepAlive).pauses;
    pauses.keep_alive_hold = Duration::from_secs(1);
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    keep_alive(&mut session, &mut [], &mut report, pauses).await;
    assert_eq!(report.failures.len(), 2);
    assert!(report.failures[0].contains("call ended before it was held"));
    assert!(report.failures[1].contains("let no keep-alive"));
    assert_eq!(page.commands().len(), 1);
}

#[tokio::test]
async fn failure_in_the_first_page_still_diagnoses_hangs_up_closes_and_judges_every_account() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut a = Page::new(|_| Err("synthetic caller disappeared".to_owned())).await;
    let mut b = Page::new(|_| Err("synthetic callee disappeared".to_owned())).await;
    let sessions = vec![
        (
            Alias::A,
            test_targets::allow(a.session(a_dir.path(), true), test_targets::A),
        ),
        (
            Alias::B,
            test_targets::allow(b.session(b_dir.path(), true), test_targets::A),
        ),
    ];
    let report = run_sessions(
        &plan(CallStep::Answer),
        sessions,
        ScenarioReport::new(ScenarioName::AbCall),
    )
    .await;
    assert_eq!(report.accounts.len(), 2);
    assert!(
        report.failures[0].contains("synthetic caller disappeared"),
        "{report:?}"
    );
    assert_eq!(
        report
            .notes
            .iter()
            .filter(|note| note.contains("couldn't hang up after the failure"))
            .count(),
        2
    );
    for commands in [a.commands(), b.commands()] {
        assert!(
            commands
                .iter()
                .any(|command| command["method"] == "Page.captureScreenshot")
        );
        assert!(commands.iter().any(|command| command["method"] == "Browser.close"));
    }
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.contains("allowed no call start"))
    );
}

async fn held_page() -> Page {
    use crate::scenario::ab_chat_full::tests::{broadcast, page_with_target};
    let mut starts = 0;
    page_with_target(&test_targets::chat(), false, false, move |command, events| {
        let expression = command["params"]["expression"].as_str().unwrap_or_default();
        if expression.contains("return buttons.length === 1") {
            return Some(Ok(evaluated(json!(test_targets::chat()))));
        }
        if command["method"] == "Runtime.evaluate" && expression.contains("e.click()")
            && (expression.contains("chat-call-audio-button") || expression.contains("chat-call-video-button"))
        {
            starts += 1;
            let request = json!({"url":CALL_START_URL,"method":"POST","headers":{},"postData":"{\"_synthetic\":true}"});
            broadcast(events,"Network.requestWillBeSent",json!({"requestId":format!("synthetic-held-{starts}"),"request":request}));
            broadcast(events,"Fetch.requestPaused",json!({"requestId":format!("fetch-held-{starts}"),"networkId":format!("synthetic-held-{starts}"),"request":request}));
            return Some(Ok(evaluated(json!(true))));
        }
        if command["method"] == "Fetch.failRequest" && command["params"]["requestId"].as_str().unwrap_or_default().starts_with("fetch-held-") {
            return Some(Ok(json!({})));
        }
        if command["method"] == "Runtime.evaluate" {
            return Some(Ok(evaluated(json!(true))));
        }
        None
    }).await
}

#[tokio::test]
async fn held_audio_and_video_starts_are_really_denied_before_each_retry_screen_is_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let mut peer = held_page().await;
    let (session, pid) = peer.managed_session(dir.path(), true);
    let session = test_targets::allow(session, test_targets::A);
    peer.send(
        "Page.frameNavigated",
        json!({"frame":{"id":"synthetic-frame","url":crate::session::TEAMS_URL}}),
    );
    peer.send("Network.requestWillBeSent", json!({"requestId":"synthetic-load","request":{"method":"GET","url":"https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME","headers":{}}}));
    peer.send(
        "Network.responseReceived",
        json!({"requestId":"synthetic-load","response":{"status":200}}),
    );
    peer.send("Page.frameStoppedLoading", json!({"frameId":"synthetic-frame"}));
    std::fs::write(
        session.capture_dir().unwrap().join(EVENTS_FILE),
        format!("{}\n", connected_socket()),
    )
    .unwrap();
    let report = run_sessions(
        &plan(CallStep::StartHeld),
        vec![(Alias::A, session)],
        ScenarioReport::new(ScenarioName::AbCall),
    )
    .await;
    assert_eq!(report.accounts.len(), 1, "{report:?}");
    assert_eq!(
        report
            .notes
            .iter()
            .filter(|note| note.contains("start(s) of") && note.contains("held back"))
            .count(),
        2,
        "{report:?}"
    );
    let commands = peer.commands();
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["method"] == "Fetch.failRequest")
            .count(),
        2
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| {
                command["params"]["expression"].as_str().is_some_and(|expression| {
                    expression.contains("e.click()") && expression.contains("calling-retry-cancelbutton")
                })
            })
            .count(),
        2
    );
    assert!(commands.iter().any(|command| command["method"] == "Browser.close"));
    assert!(report.failures.is_empty(), "{report:?}");
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test]
async fn a_running_call_conflict_stops_the_ring_wait_and_ringing_diagnostics_keep_both_results() {
    let caller = tempfile::tempdir().unwrap();
    let recipient = tempfile::tempdir().unwrap();
    let body = json!({"code":409,"subCode":5704,"conversationUrl":{"Location":"https://synthetic.invalid/conv/already-running?private=omitted"}});
    let frame =
        json!({"id":1,"method":"POST","url":"/v4/f/synthetic/conversationEnd/","headers":{},"body":body.to_string()});
    let event = json!({"type":"cdp","method":"Network.webSocketFrameReceived","params":{"response":{"payloadData":format!("3:::{frame}")}}});
    std::fs::write(caller.path().join(EVENTS_FILE), format!("{event}\n")).unwrap();
    let mut peer = Page::new(|_| Err("synthetic incoming call is not shown".to_owned())).await;
    let mut sessions = [(
        Alias::B,
        test_targets::allow(peer.session(recipient.path(), true), test_targets::A),
    )];
    let mut report = ScenarioReport::new(ScenarioName::AbCall);
    let error = wait_incoming(&mut sessions, Some(caller.path()), &mut report)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already-running"), "{error}");
    assert!(!error.to_string().contains("private="));
    assert_eq!(peer.commands().len(), 1);
    note_ringing(Alias::B, &sessions[0].1, &mut report).await;
    assert_eq!(report.notes.len(), 2);
    assert!(report.notes[0].contains("no screenshot"));
    assert!(report.notes[1].ends_with("controls: null"));
}

#[tokio::test]
async fn failure_opening_a_callee_closes_the_caller_and_does_not_open_later_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let mut peer = Page::new(|_| Ok(json!({}))).await;
    let (session, pid) = peer.managed_session(dir.path(), true);
    let session = test_targets::allow(session, test_targets::A);
    let mut session = Some(session);
    let mut aliases = Vec::new();
    let error = open_all_with(
        &[Alias::A, Alias::B, Alias::C],
        &SessionOptions::fail_closed(),
        async |alias, _| {
            aliases.push(alias);
            session
                .take()
                .ok_or_else(|| SessionError::NoChrome("synthetic-refused-callee".into()))
        },
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("synthetic-refused-callee"));
    assert_eq!(aliases, [Alias::A, Alias::B]);
    assert_eq!(peer.commands().len(), 1);
    assert_eq!(peer.commands()[0]["method"], "Browser.close");
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}
