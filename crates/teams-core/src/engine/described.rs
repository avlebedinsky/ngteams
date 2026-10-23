//! The engine's actions and events as the app's diagnostic log names them (M2 Task 24a): by kind, conversation and
//! message ids, outbox rows by id, state and `clientmessageid`; never a message's text, a name or a body. A message
//! event is at `info` when the push channel brought it, at `debug` when a read did (a catch-up stores many).

use log::{Level, LevelFilter};

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

/// An event's level and line, when a log at `max` writes it: the line is formatted only then, and each event is
/// judged at its own level (a failure is logged at `warn` too, a message only at `info` or `debug`).
pub(super) fn logged(event: &Event, max: LevelFilter) -> Option<(Level, String)> {
    let level = level(event);
    (level <= max).then(|| (level, line(event)))
}

/// An event's level: `warn` for a failure, `debug` for what a read stores in bulk and for typing, `info` otherwise.
fn level(event: &Event) -> Level {
    match event {
        Event::Message { source, .. } => match source {
            Source::Delivery | Source::Replay => Level::Info,
            Source::CatchUp | Source::History => Level::Debug,
        },
        Event::Typing { .. } => Level::Debug,
        Event::Failed { .. } | Event::CatchUpFailed { .. } => Level::Warn,
        _ => Level::Info,
    }
}

/// An event's line.
fn line(event: &Event) -> String {
    match event {
        Event::Message {
            conversation,
            id,
            change,
            applied,
            source,
            ..
        } => format!("message {id} in {conversation}: {change:?}, {applied:?}, from {source:?}"),
        Event::Typing {
            conversation, typing, ..
        } => format!("typing {typing} in {conversation}"),
        Event::Queued(row) | Event::Outbox(row) => format!(
            "outbox row {} ({:?}, clientmessageid {}): {:?}",
            row.id, row.kind, row.client_message_id, row.state
        ),
        Event::Failed { during, error } => format!("failed {during}: {error}"),
        Event::CatchUpFailed { cause, error } => format!("catch-up ({cause:?}) failed: {error}"),
        other => plain(other),
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
        Event::Presence { mri, availability } => format!("presence of {mri}: {availability:?}"),
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

#[cfg(test)]
mod tests {
    use log::{Level, LevelFilter};

    use super::super::{EngineError, Event};
    use super::logged;

    fn failed() -> Event {
        Event::Failed {
            during: "indexing stored messages for search",
            error: EngineError::Deleted {
                message: "1700000000000".to_owned(),
            },
        }
    }

    #[test]
    fn a_failure_is_logged_at_warn_and_an_info_event_isn_t() {
        let (level, line) = logged(&failed(), LevelFilter::Warn).expect("a warning at warn");
        assert_eq!(level, Level::Warn);
        assert!(line.starts_with("failed indexing stored messages for search"), "{line}");
        assert!(logged(&Event::Teams, LevelFilter::Warn).is_none());
        assert_eq!(
            logged(&Event::Teams, LevelFilter::Info).map(|(level, _)| level),
            Some(Level::Info)
        );
    }

    #[test]
    fn a_presence_change_is_logged_at_info_by_mri_and_availability() {
        let event = Event::Presence {
            mri: "8:orgid:00000000-0000-0000-0000-00000000000b".to_owned(),
            availability: crate::presence::Availability::Away,
        };
        let (level, line) = logged(&event, LevelFilter::Info).expect("a line at info");
        assert_eq!(level, Level::Info);
        assert_eq!(line, "presence of 8:orgid:00000000-0000-0000-0000-00000000000b: Away");
        assert!(logged(&event, LevelFilter::Warn).is_none());
    }

    #[test]
    fn nothing_is_logged_below_the_event_s_level() {
        assert!(logged(&failed(), LevelFilter::Error).is_none());
        assert!(logged(&failed(), LevelFilter::Off).is_none());
    }
}
