//! The account's calendar events as the calendar view last read them (schema 16), so a later run shows them at once
//! while it reads them again. Each read of a window replaces the stored events that window overlaps: an event the
//! service no longer gives there (deleted, moved away, a declined occurrence it dropped) goes, one it gives is kept as
//! it came. An event is kept as its JSON object, as text: `teams-proto` reads it, the store only places it.

use std::fmt;

use rusqlite::params;

use crate::{Store, StoreError};

/// One stored event. `Debug` leaves its entry out (its subject, its people).
#[derive(Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// Its id (`objectId`).
    pub id: String,
    /// Where it starts and ends, ms since the epoch.
    pub start_ms: i64,
    pub end_ms: i64,
    /// The event as the service gave it.
    pub entry: String,
}

impl fmt::Debug for StoredEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredEvent")
            .field("id", &self.id)
            .field("start_ms", &self.start_ms)
            .field("end_ms", &self.end_ms)
            .finish_non_exhaustive()
    }
}

/// An event overlaps the window `?1..?2` when it starts before the window ends and ends after it starts (one with no
/// length, when it starts in it).
const OVERLAPS: &str = "start_ms < ?2 AND (end_ms > ?1 OR start_ms >= ?1)";

impl Store {
    /// The stored events that overlap `start_ms..end_ms`, by start.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn calendar_events(&self, start_ms: i64, end_ms: i64) -> Result<Vec<StoredEvent>, StoreError> {
        let sql =
            format!("SELECT id, start_ms, end_ms, entry FROM calendar_events WHERE {OVERLAPS} ORDER BY start_ms, id");
        let events = self
            .conn
            .prepare_cached(&sql)?
            .query_map(params![start_ms, end_ms], |row| {
                Ok(StoredEvent {
                    id: row.get(0)?,
                    start_ms: row.get(1)?,
                    end_ms: row.get(2)?,
                    entry: row.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(events)
    }

    /// Replaces the stored events that overlap `start_ms..end_ms` with `events`, the window's read, in one
    /// transaction; an event stored elsewhere under the id of one of `events` (one that moved) is replaced too, and
    /// of an id `events` gives twice the last is kept.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails; nothing changed then.
    pub fn replace_calendar_window(
        &mut self,
        start_ms: i64,
        end_ms: i64,
        events: &[StoredEvent],
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!("DELETE FROM calendar_events WHERE {OVERLAPS}"),
            params![start_ms, end_ms],
        )?;
        {
            let mut row = tx.prepare_cached(
                "INSERT INTO calendar_events (id, start_ms, end_ms, entry) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET start_ms = excluded.start_ms, end_ms = excluded.end_ms,
                 entry = excluded.entry",
            )?;
            for event in events {
                row.execute(params![event.id, event.start_ms, event.end_ms, event.entry])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
