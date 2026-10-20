-- An outbox row's budget counts from when it was made or last resent (Store::resend), not over its whole life.

-- the attempts it had when it was last resent: its attempts since then are `attempts - attempts_at_resend`
ALTER TABLE outbox ADD COLUMN attempts_at_resend INTEGER NOT NULL DEFAULT 0;

-- checks before a new attempt that couldn't sync its conversation, since it was made or last resent
ALTER TABLE outbox ADD COLUMN failed_checks INTEGER NOT NULL DEFAULT 0;
