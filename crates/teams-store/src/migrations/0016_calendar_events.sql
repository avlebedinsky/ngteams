-- Schema 16: the account's calendar events as the calendar view last read them, each window read replacing the
-- events it overlaps, so the calendar shows them at once in a later run while it reads them again. `entry` is the
-- event's JSON object as the service gave it (teams-proto reads it again); `start_ms` and `end_ms` place it.
CREATE TABLE calendar_events (
    id TEXT PRIMARY KEY,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    entry TEXT NOT NULL
) STRICT;
CREATE INDEX calendar_events_by_start ON calendar_events (start_ms);
