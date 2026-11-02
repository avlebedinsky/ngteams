-- Schema 14: what "Your teams and channels" and Manage team show of a team (`docs/protocol/teams.md`): its description,
-- who can join it, how many people it has, the account's place on it and whether its channels are hidden; and whether
-- each channel is in the account's list.
ALTER TABLE teams ADD COLUMN tenant TEXT NOT NULL DEFAULT '';
ALTER TABLE teams ADD COLUMN description TEXT NOT NULL DEFAULT '';
ALTER TABLE teams ADD COLUMN public INTEGER NOT NULL DEFAULT 0 CHECK (public IN (0, 1));
ALTER TABLE teams ADD COLUMN people INTEGER NOT NULL DEFAULT 0;
ALTER TABLE teams ADD COLUMN guest INTEGER NOT NULL DEFAULT 0 CHECK (guest IN (0, 1));
ALTER TABLE teams ADD COLUMN dynamic_membership INTEGER NOT NULL DEFAULT 0 CHECK (dynamic_membership IN (0, 1));
ALTER TABLE teams ADD COLUMN tenant_wide INTEGER NOT NULL DEFAULT 0 CHECK (tenant_wide IN (0, 1));
ALTER TABLE teams ADD COLUMN channel_only INTEGER NOT NULL DEFAULT 0 CHECK (channel_only IN (0, 1));
ALTER TABLE teams ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0, 1));
ALTER TABLE teams ADD COLUMN picture_etag TEXT;
ALTER TABLE team_channels ADD COLUMN shown INTEGER NOT NULL DEFAULT 1 CHECK (shown IN (0, 1));

-- The stored lists hold none of these: they are read again the next time a view asks, kept shown until then.
UPDATE teams_read SET read_at = 0;
