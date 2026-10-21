//! What the commands print, when what the service sent carries terminal controls: an escape sequence, a carriage
//! return, a bidirectional override. The text output shows each as its escape, the JSON output as a JSON escape of the
//! same value; neither prints one raw.

use serde_json::Value;
use teams_core::{Event, MessageChange, Source};
use teams_headless::chat::LabNames;
use teams_headless::print::{
    conversation_line, event_json, event_line, is_terminal_control, json_line, message_line, visible,
};
use teams_store::{Applied, Conversation, ConversationKind, Message};

/// Clears the screen, returns to the line's start to print over the prefix, and reverses what follows.
const HOSTILE: &str = "hi\u{1b}[2J\rfake\u{202e}txet\u{7f}\u{9b}";
const SHOWN: &str = r"hi\u{1b}[2J\u{d}fake\u{202e}txet\u{7f}\u{9b}";

fn message(content: &str) -> Message {
    Message {
        conversation_id: "19:synthetic@thread.v2".to_owned(),
        id: "1700000000001".to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some("8:orgid:synthetic\u{1b}]0;title\u{7}".to_owned()),
        sender_name: None,
        message_type: "Text".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: 1_700_000_000_001,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn raw_controls(text: &str) -> Vec<char> {
    text.chars()
        .filter(|character| is_terminal_control(*character))
        .collect()
}

#[test]
fn every_terminal_control_is_shown_as_its_escape() {
    assert_eq!(visible(HOSTILE), SHOWN);
    assert_eq!(
        visible("plain text, ünïcode and emoji 🙂"),
        "plain text, ünïcode and emoji 🙂"
    );
    for control in [
        '\u{0}', '\t', '\n', '\u{1f}', '\u{7f}', '\u{80}', '\u{9f}', '\u{61c}', '\u{200e}', '\u{2069}',
    ] {
        assert!(is_terminal_control(control), "{control:?}");
    }
    for plain in [' ', '~', '\u{a0}', '\u{200d}', '\u{2070}'] {
        assert!(!is_terminal_control(plain), "{plain:?}");
    }
}

#[test]
fn a_message_s_text_and_sender_print_no_raw_control() {
    let line = message_line(&message(&format!("{HOSTILE}\nsecond")), &[], &LabNames::default());
    assert!(raw_controls(&line).is_empty(), "{line:?}");
    assert!(line.contains(&format!(": {SHOWN} / second")), "{line}");
    assert!(line.contains(r"synthetic\u{1b}]0;title\u{7}"), "{line}");
}

#[test]
fn a_conversation_s_topic_prints_no_raw_control() {
    let conversation = Conversation {
        id: "19:synthetic@thread.v2".to_owned(),
        kind: ConversationKind::Group,
        topic: Some(HOSTILE.to_owned()),
        version: 1,
        last_activity: None,
    };
    let line = conversation_line(&conversation, &[], &LabNames::default());
    assert_eq!(line, format!("- group 19:synthetic@thread.v2 {SHOWN}"));
}

fn hostile_event() -> Event {
    Event::Message {
        conversation: "19:synthetic@thread.v2".to_owned(),
        id: "1700000000001".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(message(HOSTILE))),
        source: Source::Delivery,
    }
}

#[test]
fn an_event_as_json_escapes_every_control_and_keeps_its_value() {
    let names = LabNames::default();
    let value = event_json(&hostile_event(), 1, &names);
    let line = json_line(&value);
    assert!(raw_controls(&line).is_empty(), "{line:?}");
    assert!(
        line.contains(r#""text":"hi\u001b[2J\rfake\u202etxet\u007f\u009b""#),
        "{line}"
    );
    let read: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(read, value, "the same JSON value");
    assert_eq!(read["text"], HOSTILE);
}

#[test]
fn an_event_as_text_prints_no_raw_control() {
    let line = event_line(&hostile_event(), 1, &LabNames::default());
    assert!(raw_controls(&line).is_empty(), "{line:?}");
    assert!(
        line.contains(r#" text="hi\u001b[2J\rfake\u202etxet\u007f\u009b""#),
        "{line}"
    );
}
