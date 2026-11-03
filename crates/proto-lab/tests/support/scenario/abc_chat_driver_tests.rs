//! Creation and messaging flows through real armed synthetic sessions, including the denied first-post contract.

use std::time::Duration;

use serde_json::json;

use super::*;
use crate::browser::Display;
use crate::scenario::ab_call::CallStep;
use crate::scenario::ab_chat_full::tests::{broadcast, page, page_with};

const CHAT: &str = "19:synthetic-ab@thread.v2";
const NONCE: &str = "synthetic-group-run";

fn options() -> RunOptions {
    RunOptions {
        display: Display::Headless,
        load_timeout: Duration::from_secs(1),
        call: CallStep::Answer,
    }
}

#[tokio::test]
async fn creation_records_only_the_group_named_by_this_runs_first_post_deny() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().join("lab-data"));
    std::fs::create_dir(paths.data_dir()).unwrap();
    let mut peer = page(false, false).await;
    let mut session = peer.session(dir.path(), true);
    hold_first_message(&session, NONCE);
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);
    let mut created = None;
    tokio::time::timeout(
        Duration::from_secs(5),
        create_steps(
            &mut session,
            &["8:orgid:synthetic-b", "8:orgid:synthetic-c"],
            NONCE,
            options(),
            &mut report,
            &mut created,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(created.as_deref(), Some(CHAT));
    assert_eq!(session.held_requests().len(), 1);
    assert_eq!(
        session.held_requests()[0].label.as_deref(),
        Some(THIS_RUN_FIRST_MESSAGE)
    );
    record_creation(&paths, created, Ok(()), &mut report);
    assert_eq!(LabChats::load(&paths).unwrap().abc.as_deref(), Some(CHAT));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("first message was denied in the capture and failed"))
    );
    assert!(!report.failed(), "{report:?}");
}

#[tokio::test]
async fn the_group_is_recorded_even_when_the_denied_post_fails_with_the_wrong_page_error() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().join("lab-data"));
    std::fs::create_dir(paths.data_dir()).unwrap();
    let mut peer = page_with(false, false, |command, events| {
        if command["method"] != "Fetch.failRequest" {
            return None;
        }
        let id = command["params"]["requestId"]
            .as_str()
            .unwrap()
            .strip_prefix("paused-")
            .unwrap();
        broadcast(
            events,
            "Network.loadingFailed",
            json!({"requestId":format!("synthetic-{id}"),"errorText":"net::ERR_FAILED"}),
        );
        Some(Ok(json!({})))
    })
    .await;
    let mut session = peer.session(dir.path(), true);
    hold_first_message(&session, NONCE);
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);
    let mut created = None;
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        create_steps(
            &mut session,
            &["8:orgid:synthetic-b", "8:orgid:synthetic-c"],
            NONCE,
            options(),
            &mut report,
            &mut created,
        ),
    )
    .await
    .unwrap();
    assert!(outcome.is_err());
    assert_eq!(
        created.as_deref(),
        Some(CHAT),
        "the id is kept before page evidence is checked"
    );
    record_creation(&paths, created, outcome, &mut report);
    assert!(report.failed());
    assert!(report.failures[0].contains("net::ERR_FAILED"));
    assert_eq!(LabChats::load(&paths).unwrap().abc.as_deref(), Some(CHAT));
}

#[tokio::test]
async fn messaging_enforces_its_probe_before_both_other_accounts_read_the_service_history() {
    let mut peers = Vec::new();
    let mut dirs = Vec::new();
    let mut sessions = Vec::new();
    for alias in Alias::ALL {
        let mut peer = page(false, false).await;
        let dir = tempfile::tempdir().unwrap();
        sessions.push((alias, peer.session(dir.path(), true)));
        peers.push(peer);
        dirs.push(dir);
    }
    hold_first_message(&sessions[0].1, NONCE);
    let steps = MessageSteps {
        chat_id: CHAT,

        nonce: NONCE,
        options: options(),
    };
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);
    assert!(
        steps
            .run(&mut [], &mut report)
            .await
            .unwrap_err()
            .to_string()
            .contains("no session for a")
    );
    tokio::time::timeout(Duration::from_secs(5), steps.run(&mut sessions, &mut report))
        .await
        .unwrap()
        .unwrap();
    assert!(report.notes.contains(&"b shows a's message".into()));
    assert!(report.notes.contains(&"c shows a's message".into()));

    assert!(report.failures.is_empty());
    assert!(
        peers[1]
            .commands()
            .iter()
            .any(|command| command["method"] == "Page.navigate")
    );
}

#[tokio::test]
async fn creation_cleanup_records_the_id_and_always_closes_the_armed_session() {
    for failing_page in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let paths = LabPaths::new(dir.path().join("data"));
        std::fs::create_dir(paths.data_dir()).unwrap();
        let mut peer = if failing_page {
            crate::scenario::test_page::Page::new(|_| Err("synthetic creation page gone".into())).await
        } else {
            page(false, false).await
        };
        let session = peer.session(dir.path(), true);
        let report = create_session(
            &paths,
            session,
            &["8:orgid:synthetic-b", "8:orgid:synthetic-c"],
            NONCE,
            options(),
        )
        .await;
        assert_eq!(report.accounts.len(), 1);
        assert!(
            peer.commands()
                .iter()
                .any(|command| command["method"] == "Browser.close")
        );
        assert!(
            report
                .failures
                .iter()
                .any(|failure| failure.contains("didn't confirm closing"))
        );
        assert_eq!(LabChats::load(&paths).unwrap().abc.is_some(), !failing_page);
        if failing_page {
            assert!(
                report
                    .failures
                    .iter()
                    .any(|failure| failure.contains("synthetic creation page gone"))
            );
            assert!(
                report
                    .notes
                    .iter()
                    .any(|note| note.contains("no screenshot of the failure"))
            );
        } else {
            assert_eq!(report.failures.len(), 1, "{report:?}");
        }
    }
}

#[tokio::test]
async fn messaging_cleanup_runs_after_success_or_a_reader_failure_and_judges_every_account() {
    for broken_reader in [false, true] {
        let mut peers = Vec::new();
        let mut dirs = Vec::new();
        let mut sessions = Vec::new();
        for alias in Alias::ALL {
            let mut peer = if broken_reader && alias == Alias::C {
                crate::scenario::test_page::Page::new(|_| Err("synthetic reader gone".into())).await
            } else {
                page(false, false).await
            };
            let dir = tempfile::tempdir().unwrap();
            sessions.push((alias, peer.session(dir.path(), true)));
            peers.push(peer);
            dirs.push(dir);
        }
        let report = message_sessions(CHAT, options(), NONCE, sessions).await.unwrap();
        assert_eq!(report.accounts.len(), 3);
        assert_eq!(
            report
                .failures
                .iter()
                .filter(|failure| failure.contains("didn't confirm closing"))
                .count(),
            3
        );
        assert!(report.notes.contains(&"b shows a's message".into()));
        assert!(peers.iter().all(|peer| {
            peer.commands()
                .iter()
                .any(|command| command["method"] == "Browser.close")
        }));
        if broken_reader {
            assert!(
                report
                    .failures
                    .iter()
                    .any(|failure| failure.contains("synthetic reader gone"))
            );
            assert!(report.notes.iter().any(|note| note.contains("page at the failure")));
        } else {
            assert_eq!(report.failures.len(), 3, "{report:?}");
        }
    }
}
