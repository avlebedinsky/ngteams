//! What the commands print, when what the service sent carries terminal controls: an escape sequence, a carriage
//! return, a bidirectional override. The text output shows each as its escape, the JSON output as a JSON escape of the
//! same value; neither prints one raw.

use serde_json::Value;
use teams_core::{Availability, Event, MessageChange, Source};
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

#[test]
fn a_presence_event_names_whose_presence_changed_and_to_what() {
    let mri = "8:orgid:synthetic-person";
    let told = |availability| Event::Presence {
        mri: mri.to_owned(),
        availability,
    };
    let value = event_json(&told(Availability::DoNotDisturb), 1, &LabNames::default());
    assert_eq!(value["event"], "presence");
    assert_eq!(value["who"], mri);
    assert_eq!(value["availability"], "DoNotDisturb");
    // a word this build doesn't name is printed as UPS gave it, without its controls
    let line = event_line(
        &told(Availability::Other("New\u{1b}[2J".to_owned())),
        1,
        &LabNames::default(),
    );
    assert!(raw_controls(&line).is_empty(), "{line:?}");
    assert!(line.contains("presence"), "{line}");
}

#[test]
fn a_profile_event_names_whose_profile_changed_and_never_their_name() {
    let mri = "8:orgid:synthetic-person";
    let value = event_json(&Event::Profile { mri: mri.to_owned() }, 1, &LabNames::default());
    assert_eq!(value["event"], "profile");
    assert_eq!(value["who"], mri);
    assert_eq!(value.as_object().map(serde_json::Map::len), Some(3), "{value}");
}

#[test]
fn a_start_refused_by_a_running_call_says_so_and_names_that_call() {
    use teams_core::{End, RemoteEnd};
    use teams_headless::print::end_word;
    let conversation =
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/Synthetic9";
    let refused = RemoteEnd {
        code: Some(409),
        sub_code: Some(5704),
        phrase: Some("Conversation cannot be created due to a failure to resolve ownership.".to_owned()),
        conversation: Some(conversation.to_owned()),
        ..RemoteEnd::default()
    };
    assert_eq!(
        end_word(&End::Remote(refused.clone())),
        format!("remote 409 chat has a running call {conversation}")
    );
    let unnamed = RemoteEnd {
        conversation: None,
        ..refused.clone()
    };
    assert_eq!(end_word(&End::Remote(unnamed)), "remote 409 chat has a running call ?");
    // any other end keeps its code and phrase
    let other = RemoteEnd {
        sub_code: Some(0),
        ..refused
    };
    assert_eq!(
        end_word(&End::Remote(other)),
        "remote 409 Conversation cannot be created due to a failure to resolve ownership."
    );
}

#[test]
fn message_event_variants_keep_sources_changes_and_applied_results() {
    for (change, word) in [
        (MessageChange::New, "new"),
        (MessageChange::Edit, "edit"),
        (MessageChange::Delete, "delete"),
        (MessageChange::Reaction, "reaction"),
        (MessageChange::Other, "other"),
    ] {
        for (source, origin) in [
            (Source::Delivery, "delivery"),
            (Source::Replay, "replay"),
            (Source::CatchUp, "catch_up"),
            (Source::History, "history"),
        ] {
            for (applied, result) in [
                (Applied::Inserted, "inserted"),
                (Applied::Updated, "updated"),
                (Applied::Ignored, "ignored"),
            ] {
                let event = Event::Message {
                    conversation: HOSTILE.into(),
                    id: "synthetic-id".into(),
                    change,
                    applied,
                    message: None,
                    source,
                };
                let value = event_json(&event, 27, &LabNames::default());
                assert_eq!(value["source"], origin);
                assert_eq!(value["replay"], source == Source::Replay);
                assert_eq!(value["change"], word);
                assert_eq!(value["applied"], result);
                assert_eq!(value["received_ms"], 27);
                assert!(value.get("text").is_none());
                assert!(raw_controls(&event_line(&event, 27, &LabNames::default())).is_empty());
            }
        }
    }
}

#[test]
fn transient_and_non_call_lifecycle_events_keep_safe_outcomes() {
    use teams_core::{Transient, TransientSent};
    let names = LabNames::default();
    for outcome in [
        TransientSent::Sent,
        TransientSent::Waiting { not_before: 123 },
        TransientSent::Denied { reason: HOSTILE.into() },
        TransientSent::Dropped { reason: HOSTILE.into() },
    ] {
        let event = Event::Transient(
            Transient::Typing {
                conversation: HOSTILE.into(),
            },
            outcome.clone(),
        );
        let json = event_json(&event, 9, &names);
        assert_eq!(json["event"], "transient");
        assert_eq!(json["kind"], "typing");
        match outcome {
            TransientSent::Sent => assert_eq!(json["outcome"], "sent"),
            TransientSent::Waiting { not_before } => assert_eq!(json["outcome"]["waiting"], not_before),
            TransientSent::Denied { reason } => assert_eq!(json["outcome"]["denied"], reason),
            TransientSent::Dropped { reason } => assert_eq!(json["outcome"]["dropped"], reason),
        }
        assert!(raw_controls(&event_line(&event, 9, &names)).is_empty());
    }
}

#[test]
fn non_call_lifecycle_events_keep_safe_outcomes() {
    use teams_core::EngineError;
    use teams_trouter::{Down, RegistrationReport, Stop};
    let names = LabNames::default();
    let events = [
        (
            Event::Connected {
                endpoint: HOSTILE.into(),
            },
            "connected",
        ),
        (Event::Registration(RegistrationReport::Registered), "registration"),
        (
            Event::Registration(RegistrationReport::Failed(HOSTILE.into())),
            "registration",
        ),
        (
            Event::Disconnected {
                down: Down::ServiceDisconnect(Some(HOSTILE.into())),
                heard: None,
            },
            "disconnected",
        ),
        (Event::PushStopped(Stop::Requested), "push_stopped"),
        (
            Event::Typing {
                conversation: HOSTILE.into(),
                from: None,
                typing: true,
            },
            "typing",
        ),
        (
            Event::Typing {
                conversation: HOSTILE.into(),
                from: Some(HOSTILE.into()),
                typing: false,
            },
            "typing_stopped",
        ),
        (
            Event::ReadHorizon {
                conversation: HOSTILE.into(),
            },
            "read_horizon",
        ),
        (
            Event::ConversationChanged {
                conversation: HOSTILE.into(),
            },
            "conversation_changed",
        ),
        (Event::Teams, "teams"),
        (
            Event::Failed {
                during: "synthetic failure",
                error: EngineError::Deleted {
                    message: HOSTILE.into(),
                },
            },
            "failed",
        ),
        (Event::Indexed { messages: 17 }, "indexed"),
    ];
    for (event, name) in events {
        assert_eq!(event_json(&event, 9, &names)["event"], name);
        assert!(raw_controls(&event_line(&event, 9, &names)).is_empty());
    }
}

#[test]
fn catch_up_events_preserve_the_reason_and_report_totals() {
    use teams_core::Cause;
    use teams_core::sync::CatchUpReport;
    let names = LabNames::default();
    for (cause, word) in [
        (Cause::Connected, "connected"),
        (Cause::MessageLoss, "message_loss"),
        (Cause::Retry, "retry"),
        (Cause::Poll, "poll"),
    ] {
        let event = Event::CaughtUp {
            cause,
            report: CatchUpReport::default(),
        };
        let json = event_json(&event, 9, &names);
        assert_eq!(json["cause"], word);
        assert_eq!(json["inserted"], 0);
    }
}

#[test]
fn write_and_queue_output_keeps_ids_errors_and_historical_attempts_separate() {
    use teams_headless::print::write_lines;
    use teams_store::{NewOutboxEntry, OutboxKind, OutboxState, Store};
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("synthetic.sqlite")).unwrap();
    let mut row = store
        .enqueue(&NewOutboxEntry {
            conversation_id: Some(HOSTILE.into()),
            kind: OutboxKind::Send,
            client_message_id: "synthetic-client-id".into(),
            payload: "PRIVATE_COMMAND_PAYLOAD".into(),
        })
        .unwrap();
    row.last_error = Some(HOSTILE.into());
    let unknown = write_lines(12, &row, None, true);
    assert!(unknown.contains(&"original_arrival_time unknown".into()));
    assert!(unknown.contains(&"message_id -".into()));
    assert!(unknown.iter().any(|line| line.starts_with("error hi")));
    row.message_id = Some("synthetic-id".into());
    row.state = OutboxState::Sent;
    let sent = write_lines(12, &row, Some(34), true);
    assert!(sent.contains(&"original_arrival_time 34".into()));
    assert!(!sent.iter().any(|line| line.starts_with("error ")));
    assert!(
        !write_lines(12, &row, None, false)
            .iter()
            .any(|line| line.starts_with("original_arrival_time"))
    );
    for (event, expected) in [(Event::Queued(row.clone()), "queued"), (Event::Outbox(row), "outbox")] {
        let value = event_json(&event, 12, &LabNames::default());
        assert_eq!(value["event"], expected);
        assert_eq!(value["kind"], "send");
        assert_eq!(value["state"], "sent");
        let line = event_line(&event, 12, &LabNames::default());
        assert!(raw_controls(&line).is_empty());
        assert!(!line.contains("PRIVATE_COMMAND_PAYLOAD"));
    }
}

#[test]
fn rich_deleted_and_unknown_message_types_and_grouped_reactions_have_stable_text() {
    use teams_headless::print::message_text;
    use teams_store::Reaction;
    let mut stored = message("<p>first</p><p>second</p>");
    stored.message_type = "RichText/Html".into();
    assert_eq!(message_text(&stored), "first\nsecond");
    stored.message_type = "ThreadActivity/Synthetic".into();
    assert_eq!(message_text(&stored), "[ThreadActivity/Synthetic]");
    stored.message_type = "RichText".into();
    stored.content = "content".into();
    assert_eq!(message_text(&stored), "content");
    stored.sender = None;
    stored.edit_time = Some(5);
    let reactions = [
        Reaction {
            key: "like".into(),
            user: "synthetic-a".into(),
            time: 1,
        },
        Reaction {
            key: "heart".into(),
            user: "synthetic-c".into(),
            time: 2,
        },
        Reaction {
            key: "like".into(),
            user: "synthetic-b".into(),
            time: 3,
        },
    ];
    let line = message_line(&stored, &reactions, &LabNames::default());
    assert!(
        line.contains("? (edited): content [like: synthetic-a, synthetic-b; heart: synthetic-c]"),
        "{line}"
    );
    stored.delete_time = Some(6);
    assert!(message_text(&stored).is_empty());
    let deleted = message_line(&stored, &[], &LabNames::default());
    assert!(deleted.contains("(deleted)"));
    assert!(!deleted.contains("content"));
}
