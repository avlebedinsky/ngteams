//! `teams-headless <alias> channel-schedule <channel> <subject>`: the channel header's Schedule a meeting from the
//! command line (`docs/protocol/calling.md`, "Schedule a meeting"): a Teams meeting on the channel's team's group
//! calendar, starting at the next full hour for half an hour, posted to the channel, its creation judged by the fence
//! against the channel's team as any write on a team is. Then it reads back what the meeting shows: the account's own
//! calendar for that day (whether it holds the meeting), and the channel's newest posts (their kinds). Prints ids, kinds
//! and flags, never a name or a text.

use std::io::{self, Write};
use std::time::Duration;

use teams_core::{EngineRequests, EventDraft, EventShowAs, EventWrite, MeetingChannel, Organizer, ZonedTime};

const HOUR_MS: i64 = 3_600_000;

/// How long the stored teams are waited for (the first read of an account's teams runs in the background).
const TEAMS_WAIT: Duration = Duration::from_secs(30);

/// The meeting's draft as of `now_ms`: `subject`, from the next full hour for half an hour, its times written in the
/// desktop's offset (the account's zone is filled in as any creation's is, and the service reads the times in it).
#[must_use]
pub fn draft(subject: &str, now_ms: i64) -> EventDraft {
    let start = now_ms.div_euclid(HOUR_MS) * HOUR_MS + HOUR_MS;
    let offset_minutes = chrono::Local::now().offset().local_minus_utc() / 60;
    let zoned = |ms| ZonedTime { ms, offset_minutes };
    EventDraft {
        subject: subject.to_owned(),
        start: zoned(start),
        end: zoned(start + HOUR_MS / 2),
        time_zone: None,
        all_day: false,
        organizer: Organizer {
            mri: String::new(),
            address: None,
            name: None,
        },
        attendees: Vec::new(),
        location: String::new(),
        body_html: String::new(),
        online: true,
        show_as: EventShowAs::Busy,
        reminder_minutes: Some(15),
        private: false,
        categories: Vec::new(),
        recurrence: None,
    }
}

/// The channel `channel` with its team's mailbox, from the stored teams (waiting for their first read).
async fn meeting_channel(engine: &EngineRequests, channel: &str) -> Option<MeetingChannel> {
    let deadline = tokio::time::Instant::now() + TEAMS_WAIT;
    loop {
        // the stored list may be older than the channel: a read of it comes while this waits
        let found = engine.teams().await.ok().flatten().and_then(|teams| {
            teams
                .into_iter()
                .find(|team| team.channels.iter().any(|known| known.id == channel))
        });
        if let Some(team) = found {
            return team.mailbox.map(|mailbox| MeetingChannel {
                channel: channel.to_owned(),
                mailbox,
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// `teams`: the account's teams as the engine holds them after `wait` (a read of them starts when the stored list is
/// old, or when a team's thread it doesn't name comes): each team's id, its channels' count, whether it names a mailbox,
/// and whether it holds `channel` when one is asked about. Prints ids and counts only.
///
/// # Errors
/// Only the output's.
pub async fn teams(
    engine: &EngineRequests,
    channel: Option<&str>,
    wait: Duration,
    out: &mut impl Write,
) -> io::Result<bool> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut listed = None;
    loop {
        if let Ok(Some(teams)) = engine.teams().await {
            let found = channel.is_none_or(|channel| {
                teams
                    .iter()
                    .any(|team| team.channels.iter().any(|known| known.id == channel))
            });
            listed = Some(teams);
            if found && channel.is_some() {
                break;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let Some(teams) = listed else {
        writeln!(out, "teams none read")?;
        return Ok(false);
    };
    for team in &teams {
        writeln!(
            out,
            "team id={} channels={} mailbox={} archived={}",
            team.id,
            team.channels.len(),
            team.mailbox.is_some(),
            team.archived
        )?;
    }
    let holds = channel.is_none_or(|channel| {
        teams
            .iter()
            .any(|team| team.channels.iter().any(|known| known.id == channel))
    });
    if let Some(channel) = channel {
        writeln!(out, "teams hold {channel}={holds}")?;
    }
    Ok(holds)
}

/// Schedules the meeting (see the module docs) as of `now_ms`; `Ok(true)` when it was created.
///
/// # Errors
/// Only the output's.
pub async fn schedule(
    engine: &EngineRequests,
    channel: &str,
    subject: &str,
    now_ms: i64,
    out: &mut impl Write,
) -> io::Result<bool> {
    let Some(place) = meeting_channel(engine, channel).await else {
        writeln!(out, "schedule channel unknown or its team has no mailbox")?;
        return Ok(false);
    };
    let draft = draft(subject, now_ms);
    let start = draft.start.ms;
    let written = match engine
        .write_event(EventWrite::CreateInChannel {
            draft,
            channel: place,
            personal_invites: false,
        })
        .await
    {
        Ok(written) => written,
        Err(error) => {
            writeln!(out, "schedule failed {}", error.kind())?;
            writeln!(out, "schedule why {error}")?;
            return Ok(false);
        }
    };
    let created = written.created.as_ref();
    writeln!(
        out,
        "scheduled id={} online={} join_link={} chat={}",
        created.map_or("unread", |event| event.id.as_str()),
        created.is_some_and(|event| event.online),
        created.is_some_and(|event| event.join_url.is_some()),
        created
            .and_then(|event| event.meeting_chat.as_deref())
            .unwrap_or("none"),
    )?;
    shows(engine, channel, subject, start, out).await?;
    Ok(true)
}

/// `--cancel`: cancels each meeting the channel's newest posts name whose title starts with `prefix` (what a lab run
/// made), as its organizer's Cancel meeting does, each cancellation judged by the fence; `Ok(true)` when every one went
/// through (none found is fine).
///
/// # Errors
/// Only the output's.
pub async fn cancel(engine: &EngineRequests, channel: &str, prefix: &str, out: &mut impl Write) -> io::Result<bool> {
    let Some(place) = meeting_channel(engine, channel).await else {
        writeln!(out, "cancel channel unknown or its team has no mailbox")?;
        return Ok(false);
    };
    if let Err(error) = engine.sync_conversation(channel).await {
        writeln!(out, "channel sync failed {}", error.kind())?;
        return Ok(false);
    }
    let page = match engine.messages(channel, None, 50).await {
        Ok(page) => page,
        Err(error) => {
            writeln!(out, "channel read failed {}", error.kind())?;
            return Ok(false);
        }
    };
    let mut fine = true;
    for message in &page.messages {
        let Some(meeting) = message
            .properties
            .as_deref()
            .and_then(teams_core::ScheduledMeeting::of_properties)
            .filter(|meeting| meeting.title.starts_with(prefix))
        else {
            continue;
        };
        let write = EventWrite::CancelInChannel {
            meeting: Box::new(meeting),
            channel: place.clone(),
            post: message.id.clone(),
            message: None,
        };
        match engine.write_event(write).await {
            Ok(_) => writeln!(out, "cancelled post={}", message.id)?,
            Err(error) => {
                fine = false;
                writeln!(out, "cancel failed post={} {}", message.id, error.kind())?;
                writeln!(out, "cancel why {error}")?;
            }
        }
    }
    Ok(fine)
}

/// Where the meeting shows once made: the account's calendar around its start, and the channel's newest posts.
async fn shows(
    engine: &EngineRequests,
    channel: &str,
    subject: &str,
    start: i64,
    out: &mut impl Write,
) -> io::Result<()> {
    // the service posts the meeting to the channel and files it in the calendars a little after the creation
    tokio::time::sleep(Duration::from_secs(10)).await;
    match engine.calendar(start - HOUR_MS, start + HOUR_MS).await {
        Ok(events) => {
            let found: Vec<_> = events.iter().filter(|event| event.subject == subject).collect();
            writeln!(
                out,
                "calendar holds={} organizer={} online={} chat={}",
                found.len(),
                found.iter().any(|event| event.is_organizer),
                found.iter().any(|event| event.online),
                found
                    .first()
                    .and_then(|event| event.meeting_chat.as_deref())
                    .unwrap_or("none")
            )?;
        }
        Err(error) => writeln!(out, "calendar read failed {}", error.kind())?,
    }
    if let Err(error) = engine.sync_conversation(channel).await {
        writeln!(out, "channel sync failed {}", error.kind())?;
        return Ok(());
    }
    match engine.messages(channel, None, 5).await {
        Ok(page) => {
            for message in &page.messages {
                let keys = message
                    .properties
                    .as_deref()
                    .and_then(|properties| serde_json::from_str::<serde_json::Value>(properties).ok())
                    .and_then(|properties| {
                        properties
                            .as_object()
                            .map(|map| map.keys().cloned().collect::<Vec<_>>().join(","))
                    })
                    .unwrap_or_default();
                writeln!(
                    out,
                    "channel post id={} type={} names_subject={} content_bytes={} properties={keys}",
                    message.id,
                    message.message_type,
                    message.content.contains(subject),
                    message.content.len()
                )?;
            }
        }
        Err(error) => writeln!(out, "channel read failed {}", error.kind())?,
    }
    Ok(())
}
