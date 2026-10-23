//! The commands, on an engine ([`EngineHandle`]) or on the store alone, printing to any writer: the binary hands them
//! the lab's engine and stdout, the tests an engine on the mock chat service and a buffer.

use std::future::Future;
use std::io::{self, Write};
use std::time::Duration;

use teams_core::{Action, CatchUpReport, EngineError, EngineHandle, Event};
use teams_store::{OutboxEntry, OutboxState, Store, StoreError};
use teams_trouter::Stop;

use crate::chat::LabNames;
use crate::print::{
    availability_word, conversation_line, event_json, event_line, json_line, message_line, now_ms, visible, write_lines,
};

/// Why a command failed.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("can't print: {0}")]
    Output(#[from] io::Error),
    #[error("the engine stopped before the write settled")]
    Stopped,
}

/// The presence of `whom` (MRIs), read once through the engine (M2 Task 17): each asked about, then their
/// `Event::Presence`s waited for until each was told or `wait` is over, then one `presence <who> <availability>` line
/// each as the engine has it then ([`availability_word`]: `Unknown` when the read failed, gave nothing for them, or
/// can't be sent). Reads only: UPS's presence read is a read sent as a `POST`, which reaches nobody.
///
/// # Errors
/// The engine's refusal, or when the output can't be written.
pub async fn presence(
    handle: &mut EngineHandle,
    whom: &[String],
    wait: Duration,
    names: &LabNames,
    out: &mut impl Write,
) -> Result<(), CommandError> {
    for mri in whom {
        handle.presence(mri.clone()).await?;
    }
    let mut untold: Vec<&String> = whom.iter().collect();
    let deadline = tokio::time::Instant::now() + wait;
    while !untold.is_empty() {
        match tokio::time::timeout_at(deadline, handle.next_event()).await {
            Ok(Some(Event::Presence { mri, .. })) => untold.retain(|asked| **asked != mri),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    for mri in whom {
        let availability = handle.presence(mri.clone()).await?;
        writeln!(out, "presence {} {}", names.show(mri), availability_word(&availability))?;
    }
    Ok(())
}

/// The stored conversations, the latest activity first, one per line ([`conversation_line`]).
///
/// # Errors
/// When the store can't be read or the output written.
pub fn list(store: &Store, limit: usize, names: &LabNames, out: &mut impl Write) -> Result<(), CommandError> {
    for conversation in store.conversations_by_activity(limit)? {
        let members = store.members(&conversation.id)?;
        writeln!(out, "{}", conversation_line(&conversation, &members, names))?;
    }
    Ok(())
}

/// A conversation's newest `limit` stored messages, the oldest of them first, each with its reactions
/// ([`message_line`]).
///
/// # Errors
/// [`StoreError::UnknownConversation`] when the store doesn't know it; when the store can't be read or the output
/// written.
pub fn show(
    store: &Store,
    conversation: &str,
    limit: usize,
    names: &LabNames,
    out: &mut impl Write,
) -> Result<(), CommandError> {
    if store.conversation(conversation)?.is_none() {
        return Err(StoreError::UnknownConversation(conversation.to_owned()).into());
    }
    let page = store.messages_before(conversation, None, limit)?;
    for message in page.messages.iter().rev() {
        let reactions = store.reactions(conversation, &message.id)?;
        writeln!(out, "{}", message_line(message, &reactions, names))?;
    }
    Ok(())
}

/// A catch-up, and its summary: `caught_up conversations=<n> inserted=<n> updated=<n> failed=<n>`, then one
/// `failed <conversation> <error>` line per conversation it couldn't sync ([`visible`]).
///
/// # Errors
/// The catch-up's error, or when the output can't be written.
pub async fn sync(handle: &EngineHandle, out: &mut impl Write) -> Result<CatchUpReport, CommandError> {
    let report = handle.sync().await?;
    writeln!(
        out,
        "caught_up conversations={} inserted={} updated={} failed={}",
        report.conversations,
        report.messages.inserted,
        report.messages.updated,
        report.failed.len()
    )?;
    for failed in &report.failed {
        writeln!(
            out,
            "{}",
            visible(&format!("failed {} {}", failed.conversation, failed.error))
        )?;
    }
    Ok(report)
}

/// Whether a row has settled: nothing more goes out for it on its own.
fn settled(state: OutboxState) -> bool {
    !matches!(state, OutboxState::Pending | OutboxState::Sending)
}

/// One write through the outbox, under the fence: its start time is taken just before it is handed to the engine.
/// A row that waits (a `Retry-After`) is waited for up to `wait`, while the engine's tick sends it. Prints
/// [`write_lines`]: for a post, the stored message's `OriginalArrivalTime` too. The row's state comes back:
/// [`OutboxState::Sent`] when it went through, anything else when it didn't (or hasn't yet, after `wait`).
///
/// # Errors
/// The engine's refusal (a message the store doesn't have, not the account's own, …), or when the output can't be
/// written.
pub async fn write(
    handle: &mut EngineHandle,
    action: Action,
    wait: Duration,
    out: &mut impl Write,
) -> Result<OutboxState, CommandError> {
    let post = match &action {
        Action::Send { conversation, .. } => Some(conversation.clone()),
        _ => None,
    };
    let started = now_ms();
    let row = handle.write(action).await?;
    let row = settle(handle, row, wait).await?;
    let arrival = match (&post, &row.message_id) {
        (Some(conversation), Some(id)) if row.state == OutboxState::Sent => handle
            .message(conversation.as_str(), id.as_str())
            .await?
            .map(|message| message.arrival_time),
        _ => None,
    };
    for line in write_lines(started, &row, arrival, post.is_some()) {
        writeln!(out, "{line}")?;
    }
    Ok(row.state)
}

/// The row as it settled, or as it stands after `wait`.
async fn settle(handle: &mut EngineHandle, row: OutboxEntry, wait: Duration) -> Result<OutboxEntry, CommandError> {
    if settled(row.state) {
        return Ok(row);
    }
    let deadline = tokio::time::sleep(wait);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return Ok(row),
            event = handle.next_event() => match event {
                Some(Event::Outbox(later)) if later.id == row.id && settled(later.state) => return Ok(later),
                Some(_) => {}
                None => return Err(CommandError::Stopped),
            },
        }
    }
}

/// How `watch` ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEnd {
    /// Its owner stopped it (a signal).
    Stopped,
    /// The push channel stopped for good, and why.
    PushStopped(Stop),
    /// The engine stopped.
    EngineStopped,
}

/// Prints each event as it is received, with its receive time: one JSON object per line with `json`
/// ([`event_json`], as [`json_line`] writes it), else a line of text ([`event_line`]); each line flushed at once. Runs until `stop` is ready, the
/// push channel stops for good, or the engine stops.
///
/// # Errors
/// When the output can't be written.
pub async fn watch(
    handle: &mut EngineHandle,
    json: bool,
    names: &LabNames,
    out: &mut impl Write,
    stop: impl Future<Output = ()>,
) -> Result<WatchEnd, CommandError> {
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => return Ok(WatchEnd::Stopped),
            event = handle.next_event() => {
                let received = now_ms();
                let Some(event) = event else {
                    return Ok(WatchEnd::EngineStopped);
                };
                let line = if json {
                    json_line(&event_json(&event, received, names))
                } else {
                    event_line(&event, received, names)
                };
                writeln!(out, "{line}")?;
                out.flush()?;
                if let Event::PushStopped(why) = event {
                    return Ok(WatchEnd::PushStopped(why));
                }
            }
        }
    }
}
