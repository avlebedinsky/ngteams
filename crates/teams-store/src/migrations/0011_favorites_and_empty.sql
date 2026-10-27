-- Schema 11: the account's favourite chats as CSA last listed them (its `Favorites` folder, in the folder's order),
-- replaced whole by each read that could read them (`favorites_read` is when the last one did), and whether the chat
-- service calls a conversation empty (`properties.isemptyconversation`; NULL while it said nothing readable).
CREATE TABLE favorites (
    position INTEGER PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL UNIQUE
) STRICT;

CREATE TABLE favorites_read (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    read_at INTEGER NOT NULL
) STRICT;

ALTER TABLE conversations ADD COLUMN empty INTEGER CHECK (empty IN (0, 1));

-- The conversations stored before this schema say nothing about being empty, and the list's sync link names only
-- the ones changed since: the list is read whole again. The stored teams are kept, but read again at the next ask, so
-- the favourites come with them.
DELETE FROM conversation_list_sync;
UPDATE teams_read SET read_at = 0;
