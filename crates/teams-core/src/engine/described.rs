//! The engine's actions and events as the app's diagnostic log names them (M2 Task 24a): by kind, conversation and
//! message ids, outbox rows by id, state and `clientmessageid`; never a message's text, a name or a body. A message
//! event is at `info` when the push channel brought it, at `debug` when a read did (a catch-up stores many).

use log::Level;

use super::{Action, Event, Source};

/// An action by its kind and ids.
pub(super) fn action(action: &Action) -> String {
    match action {
        Action::Send { conversation, .. } => format!("send to {conversation}"),
        Action::Edit {
            conversation,
            message_id,
            ..
        } => format!("edit of {message_id} in {conversation}"),
        Action::Delete {
            conversation,
            message_id,
        } => format!("delete of {message_id} in {conversation}"),
        Action::React {
            conversation,
            message_id,
            key,
        } => format!("reaction {key} on {message_id} in {conversation}"),
        Action::Unreact {
            conversation,
            message_id,
            key,
        } => format!("reaction {key} taken back on {message_id} in {conversation}"),
        Action::Read {
            conversation,
            message_id,
        } => format!("read receipt up to {message_id} in {conversation}"),
        Action::CreateChat { others } => format!("a chat with {} other(s)", others.len()),
    }
}

/// An event's level and line.
pub(super) fn event(event: &Event) -> (Level, String) {
    match event {
        Event::Message {
            conversation,
            id,
            change,
            applied,
            source,
            ..
        } => {
            let level = match source {
                Source::Delivery | Source::Replay => Level::Info,
                Source::CatchUp | Source::History => Level::Debug,
            };
            (
                level,
                format!("message {id} in {conversation}: {change:?}, {applied:?}, from {source:?}"),
            )
        }
        Event::Typing {
            conversation, typing, ..
        } => (Level::Debug, format!("typing {typing} in {conversation}")),
        Event::Queued(row) | Event::Outbox(row) => (
            Level::Info,
            format!(
                "outbox row {} ({:?}, clientmessageid {}): {:?}",
                row.id, row.kind, row.client_message_id, row.state
            ),
        ),
        Event::Failed { during, error } => (Level::Warn, format!("failed {during}: {error}")),
        Event::CatchUpFailed { cause, error } => (Level::Warn, format!("catch-up ({cause:?}) failed: {error}")),
        other => (Level::Info, plain(other)),
    }
}

/// The events whose fields hold no text of anyone's.
fn plain(event: &Event) -> String {
    match event {
        Event::Connected { endpoint } => format!("push channel up (endpoint {endpoint})"),
        Event::Registration(report) => format!("push registration: {report:?}"),
        Event::Disconnected { down, .. } => format!("push channel down: {down:?}"),
        Event::PushStopped(stop) => format!("push channel stopped: {stop:?}"),
        Event::CaughtUp { cause, report } => format!(
            "caught up ({cause:?}): {} conversation(s), {} message(s) changed, {} failed",
            report.conversations,
            report.messages.changed(),
            report.failed.len()
        ),
        Event::ReadHorizon { conversation } => format!("read horizon moved in {conversation}"),
        Event::ConversationChanged { conversation } => format!("conversation {conversation} changed"),
        Event::Transient(transient, sent) => format!("transient {transient:?}: {sent:?}"),
        Event::Profile { mri } => format!("profile of {mri} changed"),
        Event::Teams => "the teams changed".to_owned(),
        Event::Indexed { messages } => format!("search backfill over: {messages} message(s) indexed"),
        Event::Message { .. }
        | Event::Typing { .. }
        | Event::Queued(_)
        | Event::Outbox(_)
        | Event::Failed { .. }
        | Event::CatchUpFailed { .. } => String::new(),
    }
}
