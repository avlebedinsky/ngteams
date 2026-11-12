//! `chat-shapes`' helpers (M2 Task 10): its texts, the denies it expects, and how it reads typing posts, b's typing
//! indicator and reaction bodies into its notes. The page scripts are tested on a page in `scenario_dsl.rs`.

// The full Linux browser/scenario harness is outside the Windows provisioning command set.
#![cfg(unix)]

use proto_lab::scenario::ab_chat::{HELD_MARKER, chat_in_url, one_to_one_id};
use proto_lab::scenario::abc_chat::FIRST_MESSAGE_MARKER;
use proto_lab::scenario::chat_shapes::{Texts, TypingRun, control_type, indicator_changes, reaction_key, typing_posts};
use proto_lab::scenario::dsl::{RequestPattern, SeenRequest};
use serde_json::json;

const MRI_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const MRI_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const GROUP: &str = "19:0123456789abcdef0123456789abcdef@thread.v2";
const BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer/v1";

fn ab() -> String {
    one_to_one_id(MRI_A, MRI_B).unwrap_or_default()
}

#[test]
fn every_text_holds_the_run_s_nonce_and_none_is_a_probe_or_a_first_message() {
    let texts = Texts::new("n1");
    let [one, two, three] = &texts.lines;
    for text in [&texts.first, one, &texts.group_mention] {
        assert!(text.contains("n1"), "{text}");
    }
    assert!(three.contains(&texts.link), "{three}");
    assert!(texts.link.starts_with("https://example.com/"), "{}", texts.link);
    assert!(!two.contains("n1"));
    for text in [&texts.first, one, two, three, &texts.group_mention] {
        assert!(
            !text.contains(HELD_MARKER) && !text.contains(FIRST_MESSAGE_MARKER),
            "{text}"
        );
    }
    // the steps find each text in the page: none holds another
    let distinct = [&texts.first, one, &texts.group_mention];
    for (i, text) in distinct.iter().enumerate() {
        for (j, other) in distinct.iter().enumerate() {
            assert!(i == j || !text.contains(other.as_str()), "{text} holds {other}");
        }
    }
    assert_ne!(Texts::new("n2"), texts);
}

#[test]
fn a_typing_post_is_a_control_message_and_nothing_else_is() {
    assert_eq!(
        control_type(Some(
            r#"{"content":"","contenttype":"Application/Message","messagetype":"Control/Typing"}"#
        ))
        .as_deref(),
        Some("Control/Typing")
    );
    assert_eq!(
        control_type(Some(r#"{"messagetype":"Control/ClearTyping"}"#)).as_deref(),
        Some("Control/ClearTyping")
    );
    for body in [
        Some(r#"{"messagetype":"RichText/Html","content":"<p>Control/Typing</p>"}"#),
        Some(r#"{"messagetype":7}"#),
        Some("Control/Typing"),
        Some("{}"),
        None,
    ] {
        assert_eq!(control_type(body), None, "{body:?}");
    }
}

fn seen(method: &str, url: &str, body: Option<&str>, sent_ms: Option<u64>) -> SeenRequest {
    SeenRequest {
        session: None,
        request_id: "1".to_owned(),
        method: method.to_owned(),
        url: url.to_owned(),
        status: Some(201),
        failed: None,
        body: body.map(str::to_owned),
        sent_ms,
    }
}

#[test]
fn the_typing_posts_are_the_chat_s_control_posts_in_the_order_sent() -> Result<(), regex::Error> {
    let ab = ab();
    let post = RequestPattern::new(
        Some("POST"),
        &format!(r"(?i)/v1/users/ME/conversations/{}/messages(\?|$)", chat_in_url(&ab)),
    )?;
    let messages = format!("{BASE}/users/ME/conversations/{ab}/messages");
    let elsewhere = format!("{BASE}/users/ME/conversations/{GROUP}/messages");
    let typing = Some(r#"{"messagetype":"Control/Typing"}"#);
    let requests = [
        seen("POST", &messages, typing, Some(3000)),
        seen(
            "POST",
            &messages,
            Some(r#"{"messagetype":"Control/ClearTyping"}"#),
            Some(5000),
        ),
        seen("POST", &messages, typing, Some(1000)),
        // not one: another chat, a message, no time, another method
        seen("POST", &elsewhere, typing, Some(2000)),
        seen("POST", &messages, Some(r#"{"messagetype":"Text"}"#), Some(2500)),
        seen("POST", &messages, typing, None),
        seen("PUT", &messages, typing, Some(4000)),
    ];
    assert_eq!(
        typing_posts(&requests, &post),
        [
            (1000, "Control/Typing".to_owned()),
            (3000, "Control/Typing".to_owned()),
            (5000, "Control/ClearTyping".to_owned()),
        ]
    );
    Ok(())
}

#[test]
fn the_indicator_log_is_read_entry_by_entry() {
    let log = json!([
        [1000, true],
        [2500, false],
        ["3000", true],
        [4000],
        [5000, "yes"],
        [6000, true]
    ]);
    assert_eq!(indicator_changes(&log), [(1000, true), (2500, false), (6000, true)]);
    assert!(indicator_changes(&json!(null)).is_empty());
    assert!(indicator_changes(&json!({"0": [1, true]})).is_empty());
}

#[test]
fn a_reaction_body_names_its_key() {
    assert_eq!(
        reaction_key(Some(r#"{"emotions":{"key":"heart","value":1792525548203}}"#)).as_deref(),
        Some("heart")
    );
    assert_eq!(
        reaction_key(Some(r#"{"emotions":{"key":"yes-tone3"}}"#)).as_deref(),
        Some("yes-tone3")
    );
    for body in [
        Some(r#"{"emotions":{"value":1}}"#),
        Some(r#"{"key":"like"}"#),
        Some("like"),
        None,
    ] {
        assert_eq!(reaction_key(body), None, "{body:?}");
    }
}

#[test]
fn the_typing_notes_time_each_post_and_the_indicator_against_a_s_typing() {
    let run = TypingRun {
        began: 10_000,
        stopped: 40_000,
        emptied: 60_000,
        ended: 75_000,
        posts: vec![
            (10_200, "Control/Typing".to_owned()),
            (18_300, "Control/Typing".to_owned()),
            (26_400, "Control/Typing".to_owned()),
            (61_000, "Control/ClearTyping".to_owned()),
        ],
        indicator: vec![(10_900, true), (34_400, false)],
    };
    assert_eq!(
        run.notes(),
        [
            "typing: a typed for 30000 ms: 3 typing post(s), at +200, +8300, +16400 ms after its first character; gaps \
             (ms): 8100, 8100",
            "typing: after a stopped (text left in the box), for 20000 ms: no typing post",
            "typing: after a emptied the box, for 15000 ms: Control/ClearTyping at +1000 ms",
            "typing: b's indicator: shown at +900 ms, gone at +24400 ms; it went 8000 ms after a's last Control/Typing \
             was sent",
        ]
    );
    let silent = TypingRun {
        posts: Vec::new(),
        indicator: Vec::new(),
        ..run.clone()
    };
    assert_eq!(
        silent.notes()[3],
        "typing: b's indicator: none; a sent no Control/Typing"
    );
    let stuck = TypingRun {
        indicator: vec![(9_000, false), (10_900, true)],
        ..run
    };
    assert_eq!(
        stuck.notes()[3],
        "typing: b's indicator: gone at -1000 ms, shown at +900 ms; it never went after a's last Control/Typing"
    );
}
