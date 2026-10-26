//! What the commands print: stored conversations and messages, a write's outcome, and the engine's events (as text,
//! or one JSON object per line for `watch --json`). Times are milliseconds since the Unix epoch. Nothing printed
//! carries a token: the engine's events and errors carry none. What the service sent (a message's text, a topic, an
//! MRI, an error) reaches the terminal only through [`visible`] or [`json_line`], so no control character in it acts on
//! the terminal.

use std::fmt::Write as _;

use serde_json::{Value, json};
use teams_core::{Availability, CallStatus, Cause, End, Event, MessageChange, NameFrom, Source, TransientSent};
use teams_proto::chat;
use teams_store::{Applied, Conversation, Member, Message, OutboxEntry, OutboxState, Reaction};
use teams_trouter::RegistrationReport;

use crate::chat::LabNames;

/// Milliseconds since the Unix epoch, now.
#[must_use]
pub fn now_ms() -> i64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
}

/// Whether a character would act on a terminal, or reorder the text after it, if printed as it is: the C0 controls
/// (a line break and a tab too), DEL, the C1 controls, and the bidirectional formatting characters (the marks, the
/// embeddings and overrides, the isolates).
#[must_use]
pub fn is_terminal_control(character: char) -> bool {
    // `is_control` is the Cc category: U+0000–U+001F and U+007F–U+009F
    character.is_control()
        || matches!(
            character,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

/// `text` as a line of output shows it: every [terminal control](is_terminal_control) as its escape (`\u{1b}`), the
/// rest as it is.
#[must_use]
pub fn visible(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    for character in text.chars() {
        if is_terminal_control(character) {
            shown.extend(character.escape_unicode());
        } else {
            shown.push(character);
        }
    }
    shown
}

/// A JSON value on one line, with every [terminal control](is_terminal_control) in its strings as a JSON escape
/// (`\u001b`): serde escapes the C0 controls, this the rest. The same JSON value, never a raw control.
#[must_use]
pub fn json_line(value: &Value) -> String {
    let text = value.to_string();
    let mut line = String::with_capacity(text.len());
    for character in text.chars() {
        // outside a string JSON has only ASCII punctuation, digits, letters and spaces, so every such character is
        // inside one, where an escape stands for it
        if is_terminal_control(character) {
            // every terminal control is in the Basic Multilingual Plane: one `\uXXXX`; a write to a String can't fail
            let _ = write!(line, "\\u{:04x}", u32::from(character));
        } else {
            line.push(character);
        }
    }
    line
}

/// The text a reader sees of a message: rich text read as plain text, plain text as it is, nothing once deleted, and
/// the type in brackets for anything else (a thread event, a call's record).
#[must_use]
pub fn message_text(message: &Message) -> String {
    if message.delete_time.is_some() {
        return String::new();
    }
    match message.message_type.as_str() {
        // without the break that ends the last paragraph
        "RichText/Html" => chat::parse_html(&message.content)
            .plain_text()
            .trim_end_matches('\n')
            .to_owned(),
        "Text" | "RichText" => message.content.clone(),
        other => format!("[{other}]"),
    }
}

/// One stored conversation: `<last activity> <kind> <id> <topic, or its members>`, [`visible`].
#[must_use]
pub fn conversation_line(conversation: &Conversation, members: &[Member], names: &LabNames) -> String {
    let activity = conversation
        .last_activity
        .map_or_else(|| "-".to_owned(), |time| time.to_string());
    let about = match &conversation.topic {
        Some(topic) if !topic.is_empty() => topic.clone(),
        _ if members.is_empty() => "-".to_owned(),
        _ => {
            let shown: Vec<String> = members.iter().map(|member| names.show(&member.mri)).collect();
            format!("with {}", shown.join(", "))
        }
    };
    visible(&format!(
        "{activity} {} {} {about}",
        conversation.kind.as_str(),
        conversation.id
    ))
}

/// One stored message: `<arrival> <id> <sender>[ (edited)][ (deleted)][: <text>][ [<key>: <users>; …]]`, the
/// text's line breaks as ` / `, the rest [`visible`].
#[must_use]
pub fn message_line(message: &Message, reactions: &[Reaction], names: &LabNames) -> String {
    let sender = message
        .sender
        .as_deref()
        .map_or_else(|| "?".to_owned(), |mri| names.show(mri));
    let mut line = format!("{} {} {sender}", message.arrival_time, message.id);
    if message.edit_time.is_some() {
        line.push_str(" (edited)");
    }
    if message.delete_time.is_some() {
        line.push_str(" (deleted)");
    }
    let text = message_text(message);
    if !text.is_empty() {
        line.push_str(": ");
        line.push_str(&text.replace('\n', " / "));
    }
    if !reactions.is_empty() {
        line.push_str(" [");
        line.push_str(&reaction_summary(reactions, names));
        line.push(']');
    }
    visible(&line)
}

/// `<key>: <user>, <user>; <key>: …`, each key once, in the order first set.
fn reaction_summary(reactions: &[Reaction], names: &LabNames) -> String {
    let mut keys: Vec<(&str, Vec<String>)> = Vec::new();
    for reaction in reactions {
        let user = names.show(&reaction.user);
        match keys.iter_mut().find(|(key, _)| *key == reaction.key) {
            Some((_, users)) => users.push(user),
            None => keys.push((&reaction.key, vec![user])),
        }
    }
    keys.iter()
        .map(|(key, users)| format!("{key}: {}", users.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

/// What a write prints: its start time, the message's `OriginalArrivalTime` for a post (`arrival`: the stored
/// message's, which the service gave it), and the id it acted on (a post's new message), each on a line of its own,
/// then the row's state, and its error when it didn't go through; each line [`visible`].
#[must_use]
pub fn write_lines(started_ms: i64, row: &OutboxEntry, arrival: Option<i64>, post: bool) -> Vec<String> {
    let mut lines = vec![format!("start_ms {started_ms}")];
    if post {
        lines.push(format!(
            "original_arrival_time {}",
            arrival.map_or_else(|| "unknown".to_owned(), |time| time.to_string())
        ));
    }
    lines.push(format!("message_id {}", row.message_id.as_deref().unwrap_or("-")));
    lines.push(format!("state {}", row.state.as_str()));
    // a row that went through keeps the error of an attempt before it
    if let Some(error) = row.last_error.as_ref().filter(|_| row.state != OutboxState::Sent) {
        lines.push(format!("error {error}"));
    }
    lines.iter().map(|line| visible(line)).collect()
}

fn cause(cause: Cause) -> &'static str {
    match cause {
        Cause::Connected => "connected",
        Cause::MessageLoss => "message_loss",
        Cause::Retry => "retry",
    }
}

fn change(change: MessageChange) -> &'static str {
    match change {
        MessageChange::New => "new",
        MessageChange::Edit => "edit",
        MessageChange::Delete => "delete",
        MessageChange::Reaction => "reaction",
        MessageChange::Other => "other",
    }
}

fn applied(applied: Applied) -> &'static str {
    match applied {
        Applied::Inserted => "inserted",
        Applied::Updated => "updated",
        Applied::Ignored => "ignored",
    }
}

/// One event as a JSON object, with the time it was received (`received_ms`) and its name (`event`).
#[must_use]
pub fn event_json(event: &Event, received_ms: i64, names: &LabNames) -> Value {
    let mut value = event_fields(event, names);
    if let Value::Object(fields) = &mut value {
        fields.insert("received_ms".to_owned(), json!(received_ms));
    }
    value
}

fn event_fields(event: &Event, names: &LabNames) -> Value {
    match event {
        Event::Connected { endpoint } => json!({"event": "connected", "endpoint": endpoint}),
        Event::Registration(RegistrationReport::Registered) => json!({"event": "registration", "registered": true}),
        Event::Registration(RegistrationReport::Failed(error)) => {
            json!({"event": "registration", "registered": false, "error": error})
        }
        Event::Disconnected { down, .. } => json!({"event": "disconnected", "reason": format!("{down:?}")}),
        Event::PushStopped(stop) => json!({"event": "push_stopped", "reason": format!("{stop:?}")}),
        Event::CaughtUp { cause: why, report } => json!({
            "event": "caught_up",
            "cause": cause(*why),
            "conversations": report.conversations,
            "inserted": report.messages.inserted,
            "updated": report.messages.updated,
            "failed": report.failed.len(),
        }),
        Event::CatchUpFailed { cause: why, error } => {
            json!({"event": "catch_up_failed", "cause": cause(*why), "error": error.to_string()})
        }
        Event::Message { .. } => message_fields(event, names),
        Event::Typing {
            conversation,
            from,
            typing,
        } => json!({
            "event": if *typing { "typing" } else { "typing_stopped" },
            "conversation": conversation,
            "from": from.as_deref().map(|mri| names.show(mri)),
        }),
        Event::ReadHorizon { conversation } => json!({"event": "read_horizon", "conversation": conversation}),
        Event::ConversationChanged { conversation } => {
            json!({"event": "conversation_changed", "conversation": conversation})
        }
        Event::Queued(row) => json!({
            "event": "queued",
            "row": row.id,
            "kind": row.kind.as_str(),
            "state": row.state.as_str(),
            "conversation": row.conversation_id,
        }),
        Event::Outbox(row) => json!({
            "event": "outbox",
            "row": row.id,
            "kind": row.kind.as_str(),
            "state": row.state.as_str(),
            "conversation": row.conversation_id,
            "message_id": row.message_id,
            "error": row.last_error,
        }),
        Event::Transient(transient, sent) => json!({
            "event": "transient",
            "kind": transient.kind(),
            "conversation": transient.conversation(),
            "outcome": transient_outcome(sent),
        }),
        // whose profile changed, never their name
        Event::Profile { mri } => json!({"event": "profile", "who": names.show(mri)}),
        Event::Teams => json!({"event": "teams"}),
        Event::Presence { mri, availability } => json!({
            "event": "presence",
            "who": names.show(mri),
            "availability": availability_word(availability),
        }),
        Event::Failed { during, error } => json!({"event": "failed", "during": during, "error": error.to_string()}),
        Event::Indexed { messages } => json!({"event": "indexed", "messages": messages}),
        other => call_fields(other, names),
    }
}

/// A call's event: the calling socket's registration, an invitation that rings (its caller by lab name, never the
/// name the invitation gives), a ring's end, a call's status.
fn call_fields(event: &Event, names: &LabNames) -> Value {
    match event {
        Event::CallingRegistration(RegistrationReport::Registered) => {
            json!({"event": "calling_registration", "registered": true})
        }
        Event::CallingRegistration(RegistrationReport::Failed(error)) => {
            json!({"event": "calling_registration", "registered": false, "error": error})
        }
        Event::IncomingCall(incoming) => json!({
            "event": "incoming_call",
            "call": incoming.call.get(),
            "caller": names.show(incoming.caller.as_str()),
            "group_chat": incoming.group_chat,
        }),
        Event::RingEnded { call, end } => json!({
            "event": "ring_ended",
            "call": call.get(),
            "code": end.code,
            "phrase": end.phrase,
        }),
        Event::Voicemail { call } => json!({"event": "voicemail", "call": call.get()}),
        Event::CameraOff { call } => json!({"event": "camera_off", "call": call.get()}),
        // each by lab name, never the name the roster gives
        Event::CallParticipants { call, view } => json!({
            "event": "call_participants",
            "call": call.get(),
            "speaking": view.speaking,
            "participants": view.participants.iter().map(|participant| json!({
                "who": names.show(&participant.mri),
                "video_on": participant.video_on,
                "line": participant.line,
                "speaking": participant.speaking,
            })).collect::<Vec<_>>(),
        }),
        Event::Call { call, status } => {
            let mut fields = json!({"event": "call", "call": call.get(), "status": call_status_word(status)});
            if let CallStatus::Ended(end) = status {
                fields["end"] = json!(end_word(end));
            }
            fields
        }
        _ => json!({"event": "other"}),
    }
}

/// A call's status as printed: `placing`, `ringing`, …, `ended`.
#[must_use]
pub const fn call_status_word(status: &CallStatus) -> &'static str {
    match status {
        CallStatus::Placing => "placing",
        CallStatus::Ringing => "ringing",
        CallStatus::Joining => "joining",
        CallStatus::Answering => "answering",
        CallStatus::Declining => "declining",
        CallStatus::Connecting => "connecting",
        CallStatus::Connected => "connected",
        CallStatus::Reconnecting => "reconnecting",
        CallStatus::Leaving => "leaving",
        CallStatus::Ended(_) => "ended",
    }
}

/// How a call ended, as printed: `hung_up`, `declined`, `remote <code> <phrase>`, `failed <step>`.
#[must_use]
pub fn end_word(end: &End) -> String {
    match end {
        End::HungUp => "hung_up".to_owned(),
        End::Declined => "declined".to_owned(),
        End::Remote(remote) => format!(
            "remote {} {}",
            remote.code.map_or_else(|| "-".to_owned(), |code| code.to_string()),
            remote.phrase.as_deref().map_or_else(|| "-".to_owned(), visible)
        ),
        End::Failed(step) => format!("failed {step}"),
    }
}

/// Where a person's name came from, as printed: `graph`, `short_profile`, `message`, `known`, or `none` without a name.
#[must_use]
pub const fn name_source_word(from: Option<NameFrom>) -> &'static str {
    match from {
        Some(NameFrom::Graph) => "graph",
        Some(NameFrom::ShortProfile) => "short_profile",
        Some(NameFrom::Message) => "message",
        Some(NameFrom::Known) => "known",
        None => "none",
    }
}

/// An availability as printed: its name (`Available`, `Offline`, …), `Unknown`, or a word this build doesn't name as
/// UPS gave it ([`visible`]).
#[must_use]
pub fn availability_word(availability: &Availability) -> String {
    match availability {
        Availability::Other(word) => visible(word),
        named => format!("{named:?}"),
    }
}

/// A message event's fields (see [`event_fields`]).
fn message_fields(event: &Event, names: &LabNames) -> Value {
    let Event::Message {
        conversation,
        id,
        change: what,
        applied: how,
        message,
        source,
    } = event
    else {
        return Value::Null;
    };
    let value = json!({
        "event": "message",
        "conversation": conversation,
        "id": id,
        "change": change(*what),
        "applied": applied(*how),
        "replay": *source == Source::Replay,
        "source": match source {
            Source::Delivery => "delivery",
            Source::Replay => "replay",
            Source::CatchUp => "catch_up",
            Source::History => "history",
        },
    });
    with_message(value, message.as_deref(), names)
}

fn transient_outcome(sent: &TransientSent) -> Value {
    match sent {
        TransientSent::Sent => json!("sent"),
        TransientSent::Waiting { not_before } => json!({"waiting": not_before}),
        TransientSent::Denied { reason } => json!({"denied": reason}),
        TransientSent::Dropped { reason } => json!({"dropped": reason}),
    }
}

/// A message event's fields, with the stored message's when there is one.
fn with_message(mut value: Value, message: Option<&Message>, names: &LabNames) -> Value {
    if let (Some(message), Value::Object(fields)) = (message, &mut value) {
        let stored = json!({
            "sender": message.sender.as_deref().map(|mri| names.show(mri)),
            "arrival_time": message.arrival_time,
            "compose_time": message.compose_time,
            "edit_time": message.edit_time,
            "delete_time": message.delete_time,
            "text": message_text(message),
        });
        if let Value::Object(stored) = stored {
            fields.extend(stored);
        }
    }
    value
}

/// One event as a line of text: `<received> <event> <its fields as key=value>`, each value as [`json_line`] writes it.
#[must_use]
pub fn event_line(event: &Event, received_ms: i64, names: &LabNames) -> String {
    let fields = event_fields(event, names);
    let name = fields["event"].as_str().unwrap_or("?").to_owned();
    let mut line = format!("{received_ms} {name}");
    if let Value::Object(fields) = fields {
        for (key, value) in fields
            .into_iter()
            .filter(|(key, value)| key != "event" && !value.is_null())
        {
            // a string quoted as JSON quotes it, so a field's value never runs into the next
            line.push(' ');
            line.push_str(&key);
            line.push('=');
            line.push_str(&json_line(&value));
        }
    }
    line
}
