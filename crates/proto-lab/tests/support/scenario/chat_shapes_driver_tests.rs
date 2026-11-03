//! Offline armed sessions drive the chat-shapes decisions with synthetic UI and network observations.

use serde_json::json;

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::CallStep;
use crate::scenario::ab_chat_full::tests::page;

const CHAT: &str = "19:synthetic-ab@thread.v2";
const B: &str = "8:orgid:00000000-0000-0000-0000-00000000000b";

fn shapes() -> Shapes {
    Shapes {
        mri_a: "8:orgid:synthetic-a".into(),
        mri_b: B.into(),
        texts: Texts::new("synthetic-shapes"),

        options: RunOptions {
            display: Display::Headless,
            load_timeout: Duration::from_secs(1),
            call: CallStep::Answer,
        },
        waits: Waits {
            quiet: Duration::ZERO,
            typing: Duration::from_millis(2),
            every: Duration::from_millis(1),
            pause: Duration::ZERO,
            emptied: Duration::ZERO,
            lines: Duration::ZERO,
            reaction: Duration::ZERO,
        },
    }
}

#[test]
fn the_live_observation_intervals_remain_the_existing_durations() {
    let waits = Waits::default();
    assert_eq!(
        (waits.quiet, waits.typing, waits.every),
        (
            Duration::from_secs(20),
            Duration::from_secs(45),
            Duration::from_millis(500)
        )
    );
    assert_eq!(
        (waits.pause, waits.emptied, waits.lines, waits.reaction),
        (
            Duration::from_secs(35),
            Duration::from_secs(15),
            Duration::from_secs(5),
            Duration::from_secs(2)
        )
    );
}

#[tokio::test]
async fn typing_and_lines_drive_keys_and_report_the_other_pages_indicator() {
    let mut a = page(false, false).await;
    let mut b = page(false, false).await;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a_session = a.session(a_dir.path(), false);
    let b_session = b.session(b_dir.path(), false);
    let mut pages = Pages {
        a: &a_session,
        b: &b_session,
        drive_a: a_session.driver(),
        drive_b: b_session.driver(),
        writes: Writes::on(CHAT).unwrap(),
    };
    let mut shapes = shapes();
    shapes.texts.lines[0] = "line".into();
    let mut report = ScenarioReport::new(ScenarioName::ChatShapes);
    shapes.typing(&mut pages, &mut report).await.unwrap();
    shapes.lines(&mut pages, &mut report).await.unwrap();
    assert!(report.notes.iter().any(|note| note.starts_with("typing: a typed for")));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.starts_with("lines: b's typing indicator"))
    );
    let commands = a.commands();
    assert!(commands.iter().any(|command| command["params"]["key"] == "a"));
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["params"]["key"] == "Enter"
                && command["params"]["modifiers"] == 8
                && command["params"]["type"] == "keyDown")
            .count(),
        2
    );
    assert!(commands.iter().any(|command| command["params"]["key"] == " "));
}

#[tokio::test]
async fn settling_a_failed_shape_saves_both_pages_and_cancels_the_open_control() {
    let mut a = page(false, false).await;
    let mut b = page(false, false).await;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a_session = a.session(a_dir.path(), true);
    let b_session = b.session(b_dir.path(), true);
    let mut pages = Pages {
        a: &a_session,
        b: &b_session,
        drive_a: a_session.driver(),
        drive_b: b_session.driver(),
        writes: Writes::on(CHAT).unwrap(),
    };
    let mut report = ScenarioReport::new(ScenarioName::ChatShapes);
    settle(
        &mut pages,
        "synthetic failed step",
        Err(setup("synthetic control disappeared".into())),
        &mut report,
    )
    .await;
    settle(&mut pages, "next step", Ok(()), &mut report).await;
    assert_eq!(
        report.failures,
        ["synthetic failed step: synthetic control disappeared"]
    );
    assert!(report.notes.contains(&"next step: done".into()));
    assert_eq!(
        report
            .notes
            .iter()
            .filter(|note| note.contains("page at the failure"))
            .count(),
        2
    );
    assert!(report.notes.iter().any(|note| note.contains("a's visible data-tids")));
}

#[tokio::test]
async fn missing_lab_identity_stops_shapes_before_either_page_is_driven() {
    let mut a = page(false, false).await;
    let mut b = page(false, false).await;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut a_session = a.session(a_dir.path(), false);
    let mut b_session = b.session(b_dir.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::ChatShapes);
    let error = shapes()
        .run(
            None,
            Err("no synthetic group".into()),
            &mut a_session,
            &mut b_session,
            &mut report,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no MRI for a or b"));
    assert!(a.commands().is_empty() && b.commands().is_empty());
    assert_eq!(
        indicator_changes(&json!([[3, true], [7, false]])),
        [(3, true), (7, false)]
    );
}

#[tokio::test]
async fn the_complete_shapes_flow_proves_enforcement_and_continues_to_each_independent_shape() {
    for group in [Ok(CHAT.to_owned()), Err("no readable synthetic group".to_owned())] {
        let mut a = page(false, false).await;
        let mut b = page(false, false).await;
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let mut a_session = a.session(a_dir.path(), true);
        let mut b_session = b.session(b_dir.path(), true);
        let mut run = shapes();
        run.texts.lines[0] = "line".into();

        let mut report = ScenarioReport::new(ScenarioName::ChatShapes);
        let group_available = group.is_ok();
        tokio::time::timeout(
            Duration::from_secs(5),
            run.run(Some(CHAT), group, &mut a_session, &mut b_session, &mut report),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.failures.len(), usize::from(!group_available), "{report:?}");
        for step in ["typing", "lines and a link", "reactions"] {
            assert!(report.notes.contains(&format!("{step}: done")), "{report:?}");
        }
        if group_available {
            assert!(report.notes.contains(&"group mention: done".into()));
            assert!(report.notes.contains(&"b: shows a's mention in the a-b-c chat".into()));
        } else {
            assert!(report.failures[0].contains("no readable synthetic group"));
        }
        assert!(a_session.held_requests().is_empty());

        assert!(a_session.close().await.is_err());
        assert!(b_session.close().await.is_err());
    }
}

#[tokio::test]
async fn admitted_shape_runs_keep_partial_evidence_and_finalize_both_accounts_even_without_a_group_or_identity() {
    for (chat, group) in [
        (Some(CHAT), Ok(CHAT.to_owned())),
        (Some(CHAT), Err("synthetic group unreadable".to_owned())),
        (None, Ok(CHAT.to_owned())),
    ] {
        let mut a = page(false, false).await;
        let mut b = page(false, false).await;
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let (sender, a_pid) = a.managed_session(a_dir.path(), true);
        let (receiver, b_pid) = b.managed_session(b_dir.path(), true);
        let group_available = group.is_ok();
        let mut scenario = shapes();
        scenario.texts.lines[0] = "line".to_owned();
        let report = finish(sender, receiver, chat, group, &scenario).await;
        assert_eq!(report.accounts.len(), 2, "{report:?}");
        assert_eq!(report.accounts[0].alias, Alias::A);
        assert_eq!(report.accounts[1].alias, Alias::B);
        assert_eq!(
            report.failures.len(),
            usize::from(chat.is_none() || !group_available),
            "{report:?}"
        );
        for pid in [a_pid, b_pid] {
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
        for account in &report.accounts {
            let capture = account.capture.as_ref().unwrap();
            assert_eq!(capture.join("failure.png").is_file(), chat.is_none());
        }
        if chat.is_some() {
            assert!(report.notes.iter().any(|note| note == "reactions: done"));
        }
    }
}

#[tokio::test]
async fn a_typing_failure_is_diagnosed_but_does_not_abandon_later_shapes_or_the_other_account() {
    use crate::scenario::ab_chat_full::tests::page_with;
    let mut a = page_with(false, false, |command, _| {
        (command["method"] == "Input.dispatchKeyEvent" && command["params"]["key"] == "a")
            .then(|| Err("synthetic typing control detached".to_owned()))
    })
    .await;
    let mut b = page(false, false).await;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let (sender, _) = a.managed_session(a_dir.path(), true);
    let (receiver, _) = b.managed_session(b_dir.path(), true);
    let mut scenario = shapes();
    scenario.texts.lines[0] = "line".to_owned();
    let report = finish(sender, receiver, Some(CHAT), Ok(CHAT.to_owned()), &scenario).await;
    assert_eq!(report.failures.len(), 1, "{report:?}");
    assert!(report.failures[0].contains("typing control detached"));
    for step in ["lines and a link", "reactions", "group mention"] {
        assert!(report.notes.contains(&format!("{step}: done")), "{report:?}");
    }
    for account in &report.accounts {
        assert!(
            account
                .capture
                .as_ref()
                .unwrap()
                .join(format!("failure-{}-typing.png", account.alias))
                .is_file()
        );
    }
}

fn unsigned_config(root: &std::path::Path) -> LabConfig {
    let accounts = ["a", "b", "c"]
        .map(|alias| format!("[[accounts]]\nalias = '{alias}'\nupn = 'synthetic-{alias}@example.test'\n"));
    LabConfig::parse(&accounts.join("\n"), root).unwrap()
}

#[tokio::test]
async fn opening_shape_sessions_preserves_fail_closed_options_and_closes_both_when_identity_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut a = page(false, false).await;
    let mut b = page(false, false).await;
    let (first, a_pid) = a.managed_session(a_dir.path(), true);
    let (second, b_pid) = b.managed_session(b_dir.path(), true);
    let mut sessions = std::collections::VecDeque::from([first, second]);
    let mut opened = Vec::new();
    let report = run_with(
        &unsigned_config(root.path()),
        shapes().options,
        Err("synthetic no group".into()),
        async |alias, options| {
            opened.push(alias);
            assert!(options.capture_request_bodies);
            assert_eq!(options.capture.as_deref(), Some("chat-shapes"));
            assert!(matches!(options.media, Media::System));
            assert_eq!(options.display, Display::Headless);
            assert!(options.user_agent.is_none());
            Ok(sessions.pop_front().unwrap())
        },
    )
    .await
    .unwrap();
    assert_eq!(opened, [Alias::A, Alias::B]);
    assert_eq!(report.accounts.len(), 2);
    assert_eq!(report.failures.len(), 1, "{report:?}");
    assert!(report.failures[0].contains("no MRI for a or b"));
    for pid in [a_pid, b_pid] {
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
    for peer in [a, b] {
        assert!(
            peer.commands()
                .iter()
                .all(|command| command["method"] != "Page.navigate")
        );
    }
}

#[tokio::test]
async fn a_second_shape_session_open_failure_closes_and_reaps_the_first_before_returning_the_error() {
    let root = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut a = page(false, false).await;
    let (session, pid) = a.managed_session(dir.path(), true);
    let mut first = Some(session);
    let mut opened = Vec::new();
    let error = run_with(
        &unsigned_config(root.path()),
        shapes().options,
        Err("synthetic no group".into()),
        async |alias, _| {
            opened.push(alias);
            first
                .take()
                .ok_or_else(|| SessionError::NoChrome("synthetic-second-page".into()))
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("synthetic-second-page"));
    assert_eq!(opened, [Alias::A, Alias::B]);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(a.commands().len(), 1);
    assert_eq!(a.commands()[0]["method"], "Browser.close");
}

#[tokio::test]
async fn reaction_records_include_the_key_and_require_the_requested_method_to_be_answered_successfully() {
    use crate::scenario::ab_chat_full::tests::{broadcast, page_with};
    use crate::scenario::test_page::evaluated;
    for status in [201, 503] {
        let mut a = page_with(false, false, move |command, relay| {
            let expression = command["params"]["expression"].as_str()?;
            if !expression.contains("e.click()") || !expression.contains("message-actions-like") { return None; }
            for (id, method, path, body) in [("unrelated", "GET", "messages/synthetic", json!({})), ("reaction", "PUT", "messages/synthetic/properties?name=emotions", json!({"emotions":{"key":"like"}}))] {
                broadcast(relay, "Network.requestWillBeSent", json!({"requestId":id,"request":{"method":method,
                    "url":format!("https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/{CHAT}/{path}"),"postData":body.to_string(),"headers":{}}}));
                broadcast(relay, "Network.responseReceived", json!({"requestId":id,"response":{"status":status}}));
            }
            Some(Ok(evaluated(json!(true))))
        }).await;
        let writes = Writes::on(CHAT).unwrap();
        let outcome = react(
            &mut a.driver,
            &writes.reaction,
            "synthetic first",
            "message-actions-like",
            "PUT",
            Duration::ZERO,
        )
        .await;
        if status == 201 {
            let note = outcome.unwrap();
            assert!(note.contains("PUT key Some(\"like\") answered Some(201)"), "{note}");
            assert!(!note.contains("GET"));
        } else {
            assert!(outcome.unwrap_err().to_string().contains("503"));
        }
    }
}
