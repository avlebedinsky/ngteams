//! `teams-headless <alias> series`: proves the scheduling form's series and Scheduler against the service with an
//! appointment series of the account's own, which invites nobody (`docs/protocol/calendar.md`, "A series"): it creates
//! a weekly series (tomorrow at 06:00 UTC for half an hour, on that weekday, ending three weeks on), reads it back whole
//! and prints the recurrence the answer gives, reads the account's free and busy times and suggested times for that
//! day, and deletes the series again (unless `--keep`; `--cleanup` only deletes the probe series it finds). Prints
//! each step's outcome, never a subject or a body. The service reads an appointment back with no `attendees` list at
//! all, which the fence's event member read takes as the organizer alone, so the lab fence lets the delete through.

use std::io::{self, Write};

use teams_core::{EngineRequests, EventDraft, EventShowAs, EventWrite, Organizer, ZonedTime};
use teams_proto::{Recurrence, RecurrencePattern, Weekday};

const DAY_MS: i64 = 24 * 3_600_000;

/// The series the probe creates, as of `now_ms`: its draft (tomorrow 06:00 UTC, weekly on that weekday, three weeks).
#[must_use]
pub fn probe_draft(now_ms: i64) -> EventDraft {
    let tomorrow = now_ms.div_euclid(DAY_MS) * DAY_MS + DAY_MS;
    let start = tomorrow + 6 * 3_600_000;
    // 1970-01-01 was a Thursday (4, Sunday being 0)
    let weekday = Weekday::ALL[usize::try_from((tomorrow.div_euclid(DAY_MS) + 4).rem_euclid(7)).unwrap_or(0)];
    let utc = |ms| ZonedTime { ms, offset_minutes: 0 };
    EventDraft {
        subject: SUBJECT.to_owned(),
        start: utc(start),
        end: utc(start + 30 * 60_000),
        time_zone: None,
        all_day: false,
        organizer: Organizer {
            mri: String::new(),
            address: None,
            name: None,
        },
        attendees: Vec::new(),
        location: String::new(),
        body_html: "<p>A series made by the lab's probe, deleted again.</p>".to_owned(),
        online: false,
        show_as: EventShowAs::Busy,
        reminder_minutes: None,
        private: false,
        categories: Vec::new(),
        recurrence: Some(Recurrence {
            pattern: RecurrencePattern::Weekly {
                interval: 1,
                days: vec![weekday],
            },
            start: utc(tomorrow),
            end: Some(utc(tomorrow + 21 * DAY_MS)),
        }),
    }
}

/// The probe's subject: how its series are found again in the calendar.
const SUBJECT: &str = "lab series probe";

/// Runs the probe on `engine` (see the module docs); `Ok(true)` when every step went through. With `cleanup`, it
/// only deletes the probe series a run left in the calendar.
///
/// # Errors
/// Only the output's.
pub async fn probe(
    engine: &EngineRequests,
    now_ms: i64,
    keep: bool,
    cleanup: bool,
    out: &mut impl Write,
) -> io::Result<bool> {
    if cleanup {
        let found = probe_series(engine, now_ms, out).await?;
        for id in &found {
            read_back(engine, id, out).await?;
        }
        return delete_all(engine, found, out).await;
    }
    let draft = probe_draft(now_ms);
    let start_ms = draft.start.ms;
    let created = match engine.write_event(EventWrite::Create(draft)).await {
        Ok(written) => written.created,
        Err(error) => {
            writeln!(out, "series create failed {}", error.kind())?;
            writeln!(out, "series why {error}")?;
            return Ok(false);
        }
    };
    writeln!(
        out,
        "series create ok answer={}",
        created
            .as_ref()
            .map_or("unread".to_owned(), |event| format!("{:?}", event.kind))
    )?;
    let mut found = probe_series(engine, now_ms, out).await?;
    if let Some(created) = created
        && !found.contains(&created.id)
    {
        found.push(created.id);
    }
    let mut fine = !found.is_empty();
    for id in &found {
        fine &= read_back(engine, id, out).await?;
    }
    fine &= scheduler(engine, start_ms.div_euclid(DAY_MS) * DAY_MS, out).await?;
    if keep {
        writeln!(out, "series kept {}", found.len())?;
        return Ok(fine);
    }
    Ok(delete_all(engine, found, out).await? && fine)
}

/// The probe's series in the calendar from tomorrow on (by their occurrences' series, else their own ids), the
/// occurrences counted.
async fn probe_series(engine: &EngineRequests, now_ms: i64, out: &mut impl Write) -> io::Result<Vec<String>> {
    let from = now_ms.div_euclid(DAY_MS) * DAY_MS;
    let events = match engine.calendar(from, from + 30 * DAY_MS).await {
        Ok(events) => events,
        Err(error) => {
            writeln!(out, "series calendar read failed {}", error.kind())?;
            return Ok(Vec::new());
        }
    };
    let probes: Vec<_> = events.iter().filter(|event| event.subject == SUBJECT).collect();
    let mut ids: Vec<String> = probes
        .iter()
        .map(|event| event.series_master_id.clone().unwrap_or_else(|| event.id.clone()))
        .collect();
    ids.sort();
    ids.dedup();
    let kinds: Vec<String> = probes.iter().map(|event| format!("{:?}", event.kind)).collect();
    writeln!(
        out,
        "series calendar occurrences={} series={} kinds={kinds:?}",
        probes.len(),
        ids.len()
    )?;
    Ok(ids)
}

/// Reads the series `id` back whole and prints the recurrence its answer gives.
async fn read_back(engine: &EngineRequests, id: &str, out: &mut impl Write) -> io::Result<bool> {
    match engine.calendar_event(id.to_owned()).await {
        Ok(full) => {
            let pattern = full
                .recurrence
                .as_ref()
                .map(|recurrence| format!("{:?}", recurrence.pattern));
            let ends = full.recurrence.as_ref().map(|recurrence| recurrence.end.is_some());
            writeln!(
                out,
                "series read kind={:?} pattern={pattern:?} ends={ends:?}",
                full.event.kind
            )?;
            let raw = serde_json::Value::Object(full.raw.clone());
            let attendees = raw.get("attendees").map(|attendees| {
                attendees.as_array().map(|list| {
                    list.iter()
                        .map(|attendee| attendee.get("address").map(serde_json::Value::is_string))
                        .collect::<Vec<_>>()
                })
            });
            writeln!(
                out,
                "series read organizerAddress={:?} attendees={attendees:?} fence={:?}",
                raw.get("organizerAddress").map(serde_json::Value::is_string),
                teams_fence::parse_event(id, &raw)
            )?;
            for key in ["eventRecurrencePattern", "eventRecurrenceRange"] {
                let value = full.raw.get(key).map_or("none".to_owned(), ToString::to_string);
                writeln!(out, "series read {key}={value}")?;
            }
            Ok(full.recurrence.is_some())
        }
        Err(error) => {
            writeln!(out, "series read failed {}", error.kind())?;
            Ok(false)
        }
    }
}

/// The Scheduler's reads for the account alone on the day from `day_start`.
async fn scheduler(engine: &EngineRequests, day_start: i64, out: &mut impl Write) -> io::Result<bool> {
    match engine.availability(Vec::new(), day_start, 30).await {
        Ok(read) => {
            let busy: usize = read.people.iter().map(|person| person.busy.len()).sum();
            writeln!(
                out,
                "scheduler people={} busy={busy} suggestions={}",
                read.people.len(),
                read.suggestions.len()
            )?;
            Ok(true)
        }
        Err(error) => {
            writeln!(out, "scheduler failed {}", error.kind())?;
            Ok(false)
        }
    }
}

/// Deletes each series of `ids`.
async fn delete_all(engine: &EngineRequests, ids: Vec<String>, out: &mut impl Write) -> io::Result<bool> {
    let mut fine = true;
    for id in ids {
        match engine.write_event(EventWrite::Delete { id }).await {
            Ok(_) => writeln!(out, "series delete ok")?,
            Err(error) => {
                writeln!(out, "series delete failed {}", error.kind())?;
                writeln!(out, "series why {error}")?;
                fine = false;
            }
        }
    }
    Ok(fine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_repeats_on_its_first_day_s_weekday() {
        // 2026-10-31 12:00 UTC, a Saturday: the series starts on Sunday 2026-11-01
        let draft = probe_draft(1_793_448_000_000);
        let recurrence = draft.recurrence.expect("a series");
        assert_eq!(
            recurrence.pattern,
            RecurrencePattern::Weekly {
                interval: 1,
                days: vec![Weekday::Sunday]
            }
        );
        assert_eq!(recurrence.start.iso().as_deref(), Some("2026-11-01T00:00:00+00:00"));
        assert!(draft.attendees.is_empty(), "it invites nobody");
    }
}
