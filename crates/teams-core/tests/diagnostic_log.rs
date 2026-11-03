//! The app's diagnostic log at `info` (M2 Task 24a), as two native clients of the lab write it against the mock chat
//! service and its Trouter: a sends, b receives over its push channel, a edits, a's name in its message changes and b
//! catches up, every account with a real-shaped token (a JWT, made up here) and marker text and names; the log has the
//! actions, the requests, the push channel and the outbox rows by kind and id, and a grep of every file of it finds
//! none of the markers and neither token. One test: the logger is the process's.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;

use common::{every_file, jwt, message, native, ready};
use teams_core::engine::{Action, EngineHandle};
use teams_core::sync::CatchUpReport;
use teams_mock::{AB_CHAT, ChatServiceMock, LAB_A, LAB_B};
use teams_proto::chat::MessageText;
use teams_store::OutboxState;

/// Text and names that must never reach the log at `info`.
const FIRST_TEXT: &str = "MARKERTEXTFIRST the first words";
const SECOND_TEXT: &str = "MARKERTEXTSECOND the edited words";
const A_NAME: &str = "MARKERNAMEALPHA Person";
const B_NAME: &str = "MARKERNAMEBRAVO Person";
const A_RENAMED: &str = "MARKERNAMERENAMED Person";

/// a sends to b, edits, and its name in its message changes for b's next catch-up.
async fn send_edit_and_rename(mock: &ChatServiceMock, a: &mut EngineHandle, b: &mut EngineHandle) {
    let text = |words: &str| MessageText::plain(words).unwrap();
    let action = Action::Send {
        conversation: AB_CHAT.to_owned(),
        text: text(FIRST_TEXT),
    };
    let row = a.write(action).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    let id = row.message_id.clone().unwrap();
    message(b, &id).await;
    let edit = Action::Edit {
        conversation: AB_CHAT.to_owned(),
        message_id: id.clone(),
        text: text(SECOND_TEXT),
    };
    assert_eq!(a.write(edit).await.unwrap().state, OutboxState::Sent);
    message(b, &id).await;
    let renamed = mock.change_message(AB_CHAT, &row.client_message_id, |message| {
        message["imdisplayname"] = A_RENAMED.into();
        message["version"] = "9999999999999".into();
    });
    assert!(renamed);
    let report: CatchUpReport = b.sync().await.unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
}

#[tokio::test]
async fn the_info_log_has_what_happened_and_no_text_name_or_token() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    let log = teams_log::install(&teams_log::Config {
        dir: logs.clone(),
        setting: teams_log::Level::Info,
        env: None,
        rotation: teams_log::Rotation::default(),
    })
    .unwrap();
    let mock = ChatServiceMock::start().await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    let tokens = [(LAB_A, jwt("a")), (LAB_B, jwt("b"))];
    for (account, token) in &tokens {
        assert!(mock.set_token(account, token.clone()));
    }
    let (a_dir, b_dir) = (dir.path().join("a"), dir.path().join("b"));
    let mut a = native(&mock, &trouter, LAB_A, [A_NAME, B_NAME], &a_dir);
    let mut b = native(&mock, &trouter, LAB_B, [A_NAME, B_NAME], &b_dir);
    ready(&mut a).await;
    ready(&mut b).await;
    send_edit_and_rename(&mock, &mut a, &mut b).await;
    a.shutdown();
    b.shutdown();

    assert!(log.flush_written());
    let text = every_file(&logs);
    for kept in [
        "INFO  teams_core::engine: action: send to 19:",
        "INFO  teams_core::engine: action: edit of ",
        "INFO  teams_core::outbox: outbox row ",
        "INFO  teams_proto::http: POST ",
        " -> HTTP 201 in ",
        "INFO  teams_trouter::session: push channel connected",
        "INFO  teams_core::sync: catch-up: ",
        "INFO  teams_store::schema: store migrated from schema version 0 to 1",
    ] {
        assert!(text.contains(kept), "the log has {kept:?}:\n{text}");
    }
    let tokens = tokens.map(|(_, token)| token);
    for absent in [
        FIRST_TEXT,
        SECOND_TEXT,
        A_NAME,
        B_NAME,
        A_RENAMED,
        "MARKER",
        &tokens[0],
        &tokens[1],
    ] {
        assert!(!text.contains(absent), "the log holds {absent:?}:\n{text}");
    }
    assert!(!text.contains(" DEBUG "), "nothing at debug at info:\n{text}");
}
