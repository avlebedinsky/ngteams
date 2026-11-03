//! The commands, on an engine ([`EngineHandle`]) or on the store alone, printing to any writer: the binary hands them
//! the lab's engine and stdout, the tests an engine on the mock chat service and a buffer.

use std::future::Future;
use std::io::{self, Write};
use std::time::Duration;

use teams_core::{
    Action, CallAction, CallId, CatchUpReport, EngineError, EngineHandle, EngineRequests, Event, MAX_WATCHED,
};
use teams_store::{OutboxEntry, OutboxState, Store, StoreError};
use teams_trouter::Stop;

use crate::chat::LabNames;
use crate::print::{
    availability_word, conversation_line, event_json, event_line, json_line, message_line, name_source_word, now_ms,
    visible, write_lines,
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
    #[error("{asked} people asked about, more than the engine watches at once ({most})")]
    TooMany { asked: usize, most: usize },
    #[error("the calling socket didn't register in time")]
    NotRegistered,
    #[error("the call's recording can't be read or written: {0}")]
    Recording(String),
}

/// How often `profile` asks the engine again whether a lookup is still under way: a lookup that ends without a change
/// (a failed part that left the stored profile as it was) is told by no event.
const LOOKUP_POLL: Duration = Duration::from_millis(200);

/// The presence of `whom` (MRIs), read once through the engine (M2 Task 17): each asked about, then their
/// `Event::Presence`s waited for until each was told or `wait` is over, then one `presence <who> <availability>` line
/// each as the engine has it then ([`availability_word`]: `Unknown` when the read failed, gave nothing for them, or
/// can't be sent). Reads only: UPS's presence read is a read sent as a `POST`, which reaches nobody. At most
/// [`MAX_WATCHED`] people: past it the engine drops the one asked about longest ago, and what was read of them.
///
/// # Errors
/// [`CommandError::TooMany`] for more than [`MAX_WATCHED`] people, before anyone is asked about; the engine's refusal,
/// or when the output can't be written.
pub async fn presence(
    handle: &mut EngineHandle,
    whom: &[String],
    wait: Duration,
    names: &LabNames,
    out: &mut impl Write,
) -> Result<(), CommandError> {
    if whom.len() > MAX_WATCHED {
        return Err(CommandError::TooMany {
            asked: whom.len(),
            most: MAX_WATCHED,
        });
    }
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

/// The names of `whom` (MRIs) as the engine looks them up (M2 Task 12: Graph's user record, then the middle tier's
/// profile lookup for a name Graph doesn't give): each asked about, then, for those whose lookup is under way
/// ([`Person::looking`](teams_core::Person::looking): none for one the store has fresh or whose failed lookup waits to be
/// tried again), waited until none is or `wait` is over, then one `profile <who> <source>` line each as the engine has
/// it then ([`name_source_word`]: where the name came from, `none` without one), never the name. Reads only: a lookup
/// reaches nobody, whoever it names.
///
/// # Errors
/// The engine's refusal, or when the output can't be written.
pub async fn profile(
    handle: &mut EngineHandle,
    whom: &[String],
    wait: Duration,
    names: &LabNames,
    out: &mut impl Write,
) -> Result<(), CommandError> {
    let mut looking: Vec<&String> = Vec::new();
    for mri in whom {
        if handle.person(mri.clone()).await?.looking {
            looking.push(mri);
        }
    }
    let deadline = tokio::time::Instant::now() + wait;
    while !looking.is_empty() {
        let step = deadline.min(tokio::time::Instant::now() + LOOKUP_POLL);
        // the events only wake the wait sooner: a lookup that ends without a change is told by none
        if let Ok(None) = tokio::time::timeout_at(step, handle.next_event()).await {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let mut still = Vec::with_capacity(looking.len());
        for mri in looking {
            if handle.person(mri.clone()).await?.looking {
                still.push(mri);
            }
        }
        looking = still;
    }
    for mri in whom {
        let person = handle.person(mri.clone()).await?;
        writeln!(out, "profile {} {}", names.show(mri), name_source_word(person.from))?;
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

/// Whether `watch` answers calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Answering {
    /// It answers nothing (`watch` without `--answer-lab-calls`).
    #[default]
    Nothing,
    /// `--answer-lab-calls`: it answers each invitation the engine offers ([`Event::IncomingCall`]), which the lab
    /// fence offers only from a caller that isn't a person outside the lab (a lab account, the echo service, a contact),
    /// and nothing else. The answer goes out as the
    /// engine's [`CallAction::Answer`], under the fence's own judgement again; every other invitation the engine ignored
    /// already (nothing is sent for it). The only sender of an answer but the user's own action, and it lives here alone.
    LabCalls,
    /// As [`Answering::LabCalls`], each answered call's camera turned on this long after its media connected.
    LabCallsWithCamera(Duration),
}

impl Answering {
    fn answers(self) -> bool {
        self != Self::Nothing
    }

    fn camera_after(self) -> Option<Duration> {
        match self {
            Self::LabCallsWithCamera(after) => Some(after),
            Self::Nothing | Self::LabCalls => None,
        }
    }
}

/// Prints each event as it is received, with its receive time: one JSON object per line with `json`
/// ([`event_json`], as [`json_line`] writes it), else a line of text ([`event_line`]); each line flushed at once. With
/// [`Answering::LabCalls`] each offered invitation is answered after its line is printed (`<ms> call_answer call=<id>`,
/// or `call_answer_refused` with why; with `json` as an object of that `event`). Runs until `stop` is ready, the push channel stops for good, or the engine stops.
///
/// # Errors
/// When the output can't be written.
pub async fn watch(
    handle: &mut EngineHandle,
    json: bool,
    answering: Answering,
    names: &LabNames,
    out: &mut impl Write,
    stop: impl Future<Output = ()>,
) -> Result<WatchEnd, CommandError> {
    tokio::pin!(stop);
    let mut answered = None;
    let mut camera_at = None;
    loop {
        tokio::select! {
            () = &mut stop => return Ok(WatchEnd::Stopped),
            () = crate::call::sleep_until(camera_at) => {
                camera_at = None;
                if let Some(call) = answered {
                    crate::call::camera_on(handle.requests(), call, out).await?;
                }
            }
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
                match event {
                    Event::PushStopped(why) => return Ok(WatchEnd::PushStopped(why)),
                    Event::IncomingCall(incoming) if answering.answers() => {
                        answered = answer(handle.requests(), incoming.call, json, out).await?.or(answered);
                    }
                    Event::Call { call, status: teams_core::CallStatus::Connected } if Some(call) == answered => {
                        camera_at = answering
                            .camera_after()
                            .map(|after| tokio::time::Instant::now() + after);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Answers the offered invitation `call` ([`Answering::LabCalls`]), printing what became of it: a line of text, or
/// with `json` one JSON object (`call_answer` or `call_answer_refused`, with its time as `received_ms`). The call
/// answered, if it was.
async fn answer(
    requests: EngineRequests,
    call: CallId,
    json: bool,
    out: &mut impl Write,
) -> Result<Option<CallId>, CommandError> {
    let answered = requests.call(CallAction::Answer { call }).await;
    let taken = answered.as_ref().ok().copied();
    let at = now_ms();
    let line = match (answered, json) {
        (Ok(answered), false) => format!("{at} call_answer call={}", answered.get()),
        (Ok(answered), true) => json_line(&serde_json::json!({
            "event": "call_answer",
            "call": answered.get(),
            "received_ms": at,
        })),
        (Err(error), false) => format!(
            "{at} call_answer_refused call={} why={}",
            call.get(),
            json_line(&serde_json::Value::String(error.to_string()))
        ),
        (Err(error), true) => json_line(&serde_json::json!({
            "event": "call_answer_refused",
            "call": call.get(),
            "why": error.to_string(),
            "received_ms": at,
        })),
    };
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(taken)
}

/// What the team pages read of a team and of the account, each once: how much each gave, never a name.
///
/// # Errors
/// When the engine stops, or the output can't be written.
pub async fn team_reads(
    handle: &EngineHandle,
    team: &str,
    group: &str,
    out: &mut impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let roster = handle.team_roster(team).await?;
    writeln!(
        out,
        "team-reads roster people={} owners={} named={}",
        roster.len(),
        roster.iter().filter(|member| member.owner).count(),
        roster.iter().filter(|member| member.mri != member.name).count()
    )?;
    let settings = handle.team_settings(team).await?;
    writeln!(
        out,
        "team-reads settings bytes={}",
        settings.map_or(0, |settings| settings.len())
    )?;
    let tags = handle.team_tags(team).await?;
    writeln!(
        out,
        "team-reads tags count={} owners_only={}",
        tags.tags.len(),
        tags.owners_only
    )?;
    let code = handle.team_code(team).await?;
    writeln!(out, "team-reads code present={}", code.is_some())?;
    let channels = handle.team_channels(group).await?;
    writeln!(out, "team-reads channels count={}", channels.len())?;
    let invites = handle.pending_invites().await?;
    writeln!(out, "team-reads invites count={}", invites.len())?;
    let analytics = handle
        .team_analytics(group, teams_proto::AnalyticsPeriod::LastSevenDays)
        .await?;
    writeln!(
        out,
        "team-reads analytics present={} days={}",
        analytics.is_some(),
        analytics.map_or(0, |analytics| analytics.active_users_by_day.len())
    )?;
    let all = handle
        .all_teams_analytics(teams_proto::AnalyticsPeriod::LastSevenDays)
        .await?;
    writeln!(out, "team-reads all-analytics teams={}", all.len())?;
    let joinable = handle.joinable_teams(None).await?;
    writeln!(out, "team-reads joinable count={}", joinable.len())?;
    let found = handle.joinable_teams(Some("a".to_owned())).await?;
    writeln!(out, "team-reads joinable-found count={}", found.len())?;
    let people = handle.addable_people("Lab").await?;
    writeln!(out, "team-reads addable count={}", people.len())?;
    let asking = handle.team_join_requests(group).await?;
    writeln!(out, "team-reads join-requests count={}", asking.len())?;
    let picture = handle
        .team_picture(teams_proto::TeamPicture {
            own: handle.me().strip_prefix("8:orgid:").unwrap_or_default().to_owned(),
            group: group.to_owned(),
            name: "lab".to_owned(),
            etag: String::new(),
        })
        .await?;
    writeln!(
        out,
        "team-reads picture bytes={}",
        picture.map_or(0, |picture| picture.bytes.len())
    )?;
    Ok(())
}
