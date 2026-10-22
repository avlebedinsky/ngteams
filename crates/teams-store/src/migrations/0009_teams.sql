-- Schema 9 (M2 Task 16): the account's teams and their channels as CSA last listed them, and the reply chain each
-- stored channel message belongs to.
--
-- The teams list is replaced whole by each read (`position` keeps CSA's order, a team's general channel first);
-- `teams_read` is when the last read that settled it finished.
CREATE TABLE teams (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    archived INTEGER NOT NULL CHECK (archived IN (0, 1)),
    position INTEGER NOT NULL
) STRICT;

CREATE TABLE team_channels (
    id TEXT PRIMARY KEY NOT NULL,
    team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    general INTEGER NOT NULL CHECK (general IN (0, 1)),
    position INTEGER NOT NULL
) STRICT;

CREATE INDEX team_channels_by_team ON team_channels (team_id, position);

CREATE TABLE teams_read (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    read_at INTEGER NOT NULL
) STRICT;

-- A channel message's post (its reply chain's root: the post itself for a post), as its `conversationLink` named it.
CREATE TABLE reply_roots (
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    PRIMARY KEY (conversation_id, message_id),
    FOREIGN KEY (conversation_id, message_id) REFERENCES messages (conversation_id, id) ON DELETE CASCADE
) STRICT;

-- The channels' messages stored before this schema have no post recorded: their sync starts again from the newest
-- page (the messages it brings again change nothing, and record their posts), and older pages are read again from
-- there on demand.
DELETE FROM conversation_sync WHERE conversation_id IN (SELECT id FROM conversations WHERE kind = 'channel');
