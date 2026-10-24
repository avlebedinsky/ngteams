-- An outbox row records that a read met an entry it couldn't read at all carrying the row's `clientmessageid`: the
-- row's message may have arrived, and once the read moved past the entry nothing else tells.

-- when (ms) such an entry was first seen since the row was made or last resent; none when none was
ALTER TABLE outbox ADD COLUMN unreadable_seen_at INTEGER;
