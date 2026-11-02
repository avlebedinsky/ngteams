-- Schema 13: what a team's "Add channel" needs and how a channel's row shows its type (`docs/protocol/channels.md`):
-- the team's Microsoft 365 group and mail address, whether the account owns it and may add a channel and a private
-- channel to it, and each channel's type.
ALTER TABLE teams ADD COLUMN group_id TEXT;
ALTER TABLE teams ADD COLUMN mailbox TEXT;
ALTER TABLE teams ADD COLUMN owner INTEGER NOT NULL DEFAULT 0 CHECK (owner IN (0, 1));
ALTER TABLE teams ADD COLUMN can_add_channel INTEGER NOT NULL DEFAULT 0 CHECK (can_add_channel IN (0, 1));
ALTER TABLE teams ADD COLUMN can_add_private_channel INTEGER NOT NULL DEFAULT 0
    CHECK (can_add_private_channel IN (0, 1));
ALTER TABLE team_channels ADD COLUMN kind TEXT NOT NULL DEFAULT 'standard'
    CHECK (kind IN ('standard', 'private', 'shared'));

-- The stored lists hold none of these: they are read again the next time a view asks, kept shown until then.
UPDATE teams_read SET read_at = 0;
