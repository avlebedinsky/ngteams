//! `teams-headless <alias> meetings`: the account's Teams meetings in its calendar from yesterday on, each as what
//! joining it takes (`docs/protocol/calling.md`, "Joining a meeting"): its chat, whether its join link names its
//! organizer, and with `--read` what its chat's thread names (`MeetingThread`). `--cancel <prefix>` cancels each of the
//! account's own meetings whose subject starts with `prefix` (what a lab run made), each cancellation judged by the
//! fence as any. Prints ids and flags, never a subject.

use std::io::{self, Write};

use teams_core::{CalendarEvent, EngineRequests, EventWrite, MeetingCode};

const DAY_MS: i64 = 24 * 3_600_000;

/// How the account's meetings are looked at.
#[derive(Clone, Debug, Default)]
pub struct Look {
    /// Read each meeting's chat's thread.
    pub read: bool,
    /// Cancel the account's own meetings whose subject starts with this.
    pub cancel: Option<String>,
}

/// Lists (and with `look.cancel`, cancels) the account's meetings as of `now_ms`; `Ok(true)` when every step went
/// through.
///
/// # Errors
/// Only the output's.
pub async fn meetings(engine: &EngineRequests, now_ms: i64, look: &Look, out: &mut impl Write) -> io::Result<bool> {
    let from = now_ms.div_euclid(DAY_MS) * DAY_MS - DAY_MS;
    let events = match engine.calendar(from, from + 31 * DAY_MS).await {
        Ok(events) => events,
        Err(error) => {
            writeln!(out, "meetings calendar read failed {}", error.kind())?;
            return Ok(false);
        }
    };
    let mut fine = true;
    for event in events.iter().filter(|event| event.online && !event.cancelled) {
        print_event(event, out)?;
        if look.read
            && let Some(chat) = &event.meeting_chat
        {
            fine &= read_thread(engine, chat, event, out).await?;
        }
        let ours = look
            .cancel
            .as_deref()
            .is_some_and(|prefix| event.is_organizer && event.subject.starts_with(prefix));
        if ours {
            fine &= cancel(engine, event, out).await?;
        }
    }
    Ok(fine)
}

fn print_event(event: &CalendarEvent, out: &mut impl Write) -> io::Result<()> {
    let link = event.join_url.as_deref().map_or("none", |url| {
        if MeetingCode::of_link(url).is_some() {
            "meet-code"
        } else if url.contains("/l/meetup-join/") {
            "meetup-join"
        } else {
            "other"
        }
    });
    writeln!(
        out,
        "meeting id={} chat={} organizer={} link={link} names_organizer={} kind={:?}",
        event.id,
        event.meeting_chat.as_deref().unwrap_or("none"),
        event.is_organizer,
        event.meeting.is_some(),
        event.kind
    )
}

async fn read_thread(
    engine: &EngineRequests,
    chat: &str,
    event: &CalendarEvent,
    out: &mut impl Write,
) -> io::Result<bool> {
    match engine.meeting(chat.to_owned()).await {
        Ok(Some(details)) => {
            writeln!(
                out,
                "thread chat={chat} organizer_matches={} titled={} link={}",
                event.meeting.as_ref().is_none_or(|meeting| *meeting == details.meeting),
                details.title.is_some(),
                details.join_url.is_some()
            )?;
            Ok(true)
        }
        Ok(None) => {
            writeln!(out, "thread chat={chat} names no meeting")?;
            Ok(false)
        }
        Err(error) => {
            writeln!(out, "thread chat={chat} read failed {}", error.kind())?;
            Ok(false)
        }
    }
}

async fn cancel(engine: &EngineRequests, event: &CalendarEvent, out: &mut impl Write) -> io::Result<bool> {
    let write = EventWrite::Cancel {
        id: event.series_master_id.clone().unwrap_or_else(|| event.id.clone()),
        chat: event.meeting_chat.clone(),
        message: None,
    };
    match engine.write_event(write).await {
        Ok(_) => {
            writeln!(out, "cancelled id={}", event.id)?;
            Ok(true)
        }
        Err(error) => {
            writeln!(out, "cancel failed id={} {}", event.id, error.kind())?;
            writeln!(out, "cancel why {error}")?;
            Ok(false)
        }
    }
}
