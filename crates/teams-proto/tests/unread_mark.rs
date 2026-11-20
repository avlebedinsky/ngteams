//! A conversation's "Mark as unread" and its end: the account's own `consumptionHorizonBookmark`
//! (`docs/protocol/chat.md`, "Mark as unread"). Synthetic ids and times only.

use serde_json::{Value, json};
use teams_proto::chat::Conversation;
use teams_proto::{Request, UnreadMark};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CHAT: &str = "19:synthetic-a_synthetic-b@unq.gbl.spaces";

#[test]
fn mark_as_unread_puts_the_bookmark_just_before_the_message() -> TestResult {
    let request = UnreadMark::from_message(CHAT, "1767225610000", "1111111111111111111")
        .ok_or("no request")?
        .at(1_767_225_700_000);
    assert_eq!(request.bookmark(), "1767225609999;1767225700000;1111111111111111111");
    assert_eq!(request.method(), teams_proto::Method::Put);
    assert_eq!(
        request.endpoint().segments(),
        ["v1", "users", "ME", "conversations", CHAT, "properties"]
    );
    assert_eq!(
        request.endpoint().query(),
        [("name".to_owned(), "consumptionHorizonBookmark".to_owned())]
    );
    assert_eq!(
        request.body(),
        Some(json!({"consumptionHorizonBookmark": "1767225609999;1767225700000;1111111111111111111"}))
    );
    assert!(request.is_write());
    Ok(())
}

#[test]
fn the_mark_ends_with_a_bookmark_of_zeros() {
    let request = UnreadMark::cleared(CHAT).at(1_767_225_800_000);
    assert_eq!(request.bookmark(), "0;1767225800000;0");
    assert_eq!(
        request.body(),
        Some(json!({"consumptionHorizonBookmark": "0;1767225800000;0"}))
    );
}

#[test]
fn a_message_id_that_is_not_an_arrival_number_makes_no_mark() {
    assert!(UnreadMark::from_message(CHAT, "not-a-number", "1").is_none());
    assert!(UnreadMark::from_message(CHAT, "0", "1").is_none());
}

fn listed(properties: &Value) -> TestResult<Conversation> {
    Ok(serde_json::from_value(json!({
        "id": CHAT,
        "version": 1,
        "properties": properties,
        "threadProperties": {"threadType": "chat"},
    }))?)
}

#[test]
fn the_listed_chat_carries_its_unread_bookmark() -> TestResult {
    let chat = listed(&json!({
        "consumptionhorizon": "1767225620000;1767225630000;2222222222222222222",
        "consumptionHorizonBookmark": "1767225609999;1767225700000;1111111111111111111",
    }))?;
    let bookmark = chat.unread_bookmark().ok_or("no bookmark")?;
    assert_eq!(bookmark.last_read_arrival, Some(1_767_225_609_999));
    assert_eq!(bookmark.updated, Some(1_767_225_700_000));
    assert_eq!(bookmark.client_message_id.as_deref(), Some("1111111111111111111"));
    assert_eq!(chat.horizon_bookmark(), Some(1_767_225_700_000));
    let cleared = listed(&json!({"consumptionHorizonBookmark": "0;1767225800000;0"}))?;
    assert_eq!(
        cleared
            .unread_bookmark()
            .and_then(|bookmark| bookmark.last_read_arrival),
        Some(0)
    );
    assert!(listed(&json!({}))?.unread_bookmark().is_none());
    Ok(())
}
